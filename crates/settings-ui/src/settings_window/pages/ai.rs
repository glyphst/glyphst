use super::super::SettingsWindow;
use glyphst_ai::chatgpt_subscription::ChatGptSubscription;
use glyphst_settings::{
    AiApiStyle, AiProvider, AiThinkingLevel, PageImageQuality, PageTextFormat, QuickAskPosition,
    Settings, SettingsStore,
};
use glyphst_ui::icons::{Icon, IconName};
use gpui::{
    AnyElement, App, Context, Entity, PromptButton, PromptLevel, Render, SharedString, Window, div,
    prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::DialogFooter,
    h_flex,
    input::{Escape, Input, InputEvent, InputState},
    label::Label,
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage},
    switch::Switch,
    v_flex,
};

#[derive(Clone)]
struct DraggedProvider {
    provider: AiProvider,
    name: String,
}

impl Render for DraggedProvider {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w(px(220.0))
            .gap_2()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .opacity(0.9)
            .child(
                Icon::new(IconName::GripVertical)
                    .small()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(Label::new(self.name.clone()).text_sm())
    }
}

struct AddProviderDialog {
    settings_window: Entity<SettingsWindow>,
    name: Entity<InputState>,
    api_host: Entity<InputState>,
    api_key: Entity<InputState>,
    api_style: AiApiStyle,
    error: Option<SharedString>,
}

impl AddProviderDialog {
    fn new(
        settings_window: Entity<SettingsWindow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            settings_window,
            name: cx.new(|cx| InputState::new(window, cx).placeholder("Provider name")),
            api_host: cx
                .new(|cx| InputState::new(window, cx).placeholder("https://api.example.com/v1")),
            api_key: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(crate::fl!("settings-api-key-placeholder"))
                    .masked(true)
            }),
            api_style: AiApiStyle::OpenAi,
            error: None,
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        let api_host = self.api_host.read(cx).value().trim().to_string();
        if name.is_empty() || api_host.is_empty() {
            self.error = Some("Provider name and API Host are required.".into());
            cx.notify();
            return;
        }
        let api_key = self.api_key.read(cx).value().trim().to_string();
        let api_style = self.api_style;
        SettingsWindow::update_settings(&self.settings_window, cx, move |settings| {
            settings
                .ai
                .add_custom_provider(name, api_style, api_host, api_key);
        });
        window.close_dialog(cx);
    }

    fn render_content(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let openai = entity.clone();
        let anthropic = entity;
        v_flex()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(Label::new("Name").text_sm())
                    .child(Input::new(&self.name).w_full()),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(Label::new("Compatibility").text_sm())
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("custom-provider-openai-compatible")
                                    .label("OpenAI Compatible")
                                    .outline()
                                    .selected(self.api_style == AiApiStyle::OpenAi)
                                    .on_click(move |_, _, cx| {
                                        openai.update(cx, |this, cx| {
                                            this.api_style = AiApiStyle::OpenAi;
                                            cx.notify();
                                        });
                                    }),
                            )
                            .child(
                                Button::new("custom-provider-anthropic-compatible")
                                    .label("Anthropic Compatible")
                                    .outline()
                                    .selected(self.api_style == AiApiStyle::Anthropic)
                                    .on_click(move |_, _, cx| {
                                        anthropic.update(cx, |this, cx| {
                                            this.api_style = AiApiStyle::Anthropic;
                                            cx.notify();
                                        });
                                    }),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(Label::new(crate::fl!("settings-api-host")).text_sm())
                    .child(Input::new(&self.api_host).w_full()),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(Label::new(crate::fl!("settings-api-key")).text_sm())
                    .child(Input::new(&self.api_key).w_full().mask_toggle()),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(Label::new(error).text_xs().text_color(cx.theme().danger))
            })
            .into_any_element()
    }

    fn open(settings_window: Entity<SettingsWindow>, window: &mut Window, cx: &mut App) {
        let state = cx.new(|cx| AddProviderDialog::new(settings_window.clone(), window, cx));
        let content_state = state.clone();
        let submit_state = state;
        window.open_dialog(cx, move |dialog, _, _| {
            let content_state = content_state.clone();
            let submit_state = submit_state.clone();
            dialog
                .width(px(520.0))
                .title("Add model provider")
                .content(move |content, _, cx| {
                    content.child(content_state.update(cx, |this, cx| this.render_content(cx)))
                })
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("cancel-add-model-provider")
                                .label(crate::fl!("cancel"))
                                .outline()
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("confirm-add-model-provider")
                                .label("Add provider")
                                .primary()
                                .on_click(move |_, window, cx| {
                                    submit_state.update(cx, |this, cx| {
                                        this.submit(window, cx);
                                    });
                                }),
                        ),
                )
        });
    }
}

#[derive(Clone, Copy)]
enum ModelTarget {
    Main,
    Title,
    Translation,
}

fn normalized_provider_name(draft: &str) -> Option<String> {
    let name = draft.trim();
    (!name.is_empty()).then(|| name.to_string())
}

fn stable_account_button_content(
    email: SharedString,
    sign_out: SharedString,
    visible: SharedString,
) -> AnyElement {
    div()
        .relative()
        .h_full()
        .overflow_hidden()
        .child(
            v_flex()
                .invisible()
                .child(div().flex_none().child(email))
                .child(div().flex_none().child(sign_out)),
        )
        .child(
            h_flex()
                .debug_selector(|| "chatgpt-account-button-label".to_string())
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .left_0()
                .items_center()
                .justify_center()
                .child(visible),
        )
        .into_any_element()
}

impl SettingsWindow {
    fn start_provider_rename(
        &mut self,
        provider: AiProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !provider.is_custom() {
            return;
        }
        let name = cx
            .global::<SettingsStore>()
            .settings
            .ai
            .provider_name(provider);
        let input =
            cx.new(|cx| InputState::new(window, cx).default_value(SharedString::from(name)));
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |this, input, event: &InputEvent, _, cx| {
                if !matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    return;
                }
                let value = input.read(cx).value().to_string();
                this.finish_provider_rename(provider, value, cx);
            },
        );
        self.provider_rename = Some(super::super::ProviderRenameState {
            provider,
            input: input.clone(),
            _subscription: subscription,
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn finish_provider_rename(
        &mut self,
        provider: AiProvider,
        name: String,
        cx: &mut Context<Self>,
    ) {
        if self.provider_rename.as_ref().map(|rename| rename.provider) != Some(provider) {
            return;
        }
        self.provider_rename = None;
        let Some(name) = normalized_provider_name(&name) else {
            cx.notify();
            return;
        };
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        settings.ai.provider_config_mut(provider).name = name;
        self.persist_settings(settings, cx);
    }

    fn cancel_provider_rename(&mut self, provider: AiProvider, cx: &mut Context<Self>) {
        if self.provider_rename.as_ref().map(|rename| rename.provider) == Some(provider) {
            self.provider_rename = None;
            cx.notify();
        }
    }

    fn provider_divider(provider: AiProvider, section: &'static str) -> SettingItem {
        SettingItem::render(move |_, _, cx| {
            let selector = format!("provider-divider-{}-{section}", provider.as_str());
            div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .h(px(1.0))
                .w_full()
                .flex_none()
                .bg(cx.theme().border)
        })
    }

    fn provider_api_key_item(provider: AiProvider, view: Entity<Self>) -> SettingItem {
        let field = SettingField::<SharedString>::render(move |options, window, cx| {
            let value: SharedString = cx
                .global::<SettingsStore>()
                .settings
                .ai
                .provider_config(provider)
                .api_key
                .into();
            let input_view = view.clone();
            Self::bound_text_input(
                format!("provider-api-key-{}", provider.as_str()),
                value,
                crate::fl!("settings-api-key-placeholder"),
                true,
                move |value, cx| {
                    Self::update_text_settings(&input_view, cx, move |settings| {
                        settings.ai.provider_config_mut(provider).api_key = value.to_string();
                    });
                    input_view.update(cx, |this, cx| {
                        this.invalidate_provider_operation(provider);
                        cx.notify();
                    });
                },
                options,
                window,
                cx,
            )
        });
        SettingItem::new(crate::fl!("settings-api-key"), field)
    }

    fn provider_api_host_item(provider: AiProvider, view: Entity<Self>) -> SettingItem {
        let field = SettingField::<SharedString>::render(move |options, window, cx| {
            let value: SharedString = cx
                .global::<SettingsStore>()
                .settings
                .ai
                .provider_config(provider)
                .base_url
                .unwrap_or_default()
                .into();
            let input_view = view.clone();
            Self::bound_text_input(
                format!("provider-api-host-{}", provider.as_str()),
                value,
                "https://api.example.com/v1",
                false,
                move |value, cx| {
                    Self::update_text_settings(&input_view, cx, move |settings| {
                        settings.ai.provider_config_mut(provider).base_url =
                            Some(value.to_string());
                    });
                    input_view.update(cx, |this, cx| {
                        this.invalidate_provider_operation(provider);
                        cx.notify();
                    });
                },
                options,
                window,
                cx,
            )
        });
        SettingItem::new(crate::fl!("settings-api-host"), field)
    }

    fn provider_connection_item(provider: AiProvider, view: Entity<Self>) -> SettingItem {
        SettingItem::render(move |options, _, cx| {
            Self::provider_connection_field(provider, view.clone(), options, cx)
        })
    }

    fn provider_models_item(provider: AiProvider, view: Entity<Self>) -> SettingItem {
        SettingItem::render(move |options, window, cx| {
            v_flex()
                .w_full()
                .gap_3()
                .child(
                    h_flex()
                        .debug_selector({
                            let selector = format!("provider-model-header-{}", provider.as_str());
                            move || selector.clone()
                        })
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(Label::new(crate::fl!("settings-model-list")).text_sm())
                                .child(
                                    Label::new(crate::fl!("settings-drag-to-reorder"))
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground),
                                ),
                        )
                        .child(Self::provider_add_models_button(
                            provider,
                            view.clone(),
                            options,
                            window,
                            cx,
                        )),
                )
                .child(Self::provider_model_list_field(
                    provider,
                    view.clone(),
                    options,
                    cx,
                ))
        })
        .keywords([
            crate::fl!("settings-model-list"),
            crate::fl!("settings-add-models"),
            crate::fl!("settings-add-model-id"),
            crate::fl!("settings-fetch-models"),
        ])
    }

    fn provider_order_item(
        provider: AiProvider,
        name: String,
        enabled: bool,
        view: Entity<Self>,
        cx: &mut App,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let target_view = view.clone();
        let toggle_view = view.clone();
        let rename_view = view.clone();
        let cancel_rename_view = view.clone();
        let provider_drag_view = view.clone();
        let rename_input = view
            .read(cx)
            .provider_rename
            .as_ref()
            .filter(|rename| rename.provider == provider)
            .map(|rename| rename.input.clone());
        let drag = DraggedProvider {
            provider,
            name: name.clone(),
        };
        let drag_preview = drag.clone();
        let rename_group: SharedString = format!("rename-ai-provider-{}", provider.as_str()).into();
        let provider_name = if let Some(input) = rename_input {
            div()
                .flex_1()
                .min_w_0()
                .on_action(move |_: &Escape, _, cx| {
                    cancel_rename_view.update(cx, |this, cx| {
                        this.cancel_provider_rename(provider, cx);
                    });
                })
                .child(Input::new(&input).small().w_full().min_w_0())
                .into_any_element()
        } else {
            h_flex()
                .group(rename_group.clone())
                .min_w_0()
                .gap_1()
                .child(Label::new(name).text_sm())
                .when(provider.is_custom(), |this| {
                    this.child(
                        Button::new(SharedString::from(format!(
                            "rename-ai-provider-{}",
                            provider.as_str()
                        )))
                        .icon(IconName::Pencil)
                        .tooltip("Rename provider")
                        .ghost()
                        .compact()
                        .xsmall()
                        .opacity(0.0)
                        .group_hover(rename_group, |style| style.opacity(1.0))
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            rename_view.update(cx, |this, cx| {
                                this.start_provider_rename(provider, window, cx);
                            });
                        }),
                    )
                })
                .into_any_element()
        };
        h_flex()
            .id(SharedString::from(format!(
                "ordered-ai-provider-{}",
                provider.as_str()
            )))
            .w_full()
            .min_w_0()
            .justify_between()
            .gap_3()
            .px_3()
            .py_2()
            .border_1()
            .border_color(theme.border)
            .rounded(px(6.0))
            .drag_over::<DraggedProvider>(move |style, dragged, _, _| {
                if dragged.provider == provider {
                    style
                } else {
                    style.border_t(px(2.0)).border_color(theme.primary)
                }
            })
            .on_drop(move |dragged: &DraggedProvider, _, cx| {
                let dragged = dragged.provider;
                SettingsWindow::update_settings(&target_view, cx, move |settings| {
                    let Some(from) = settings
                        .ai
                        .provider_order
                        .iter()
                        .position(|entry| *entry == dragged)
                    else {
                        return;
                    };
                    let Some(to) = settings
                        .ai
                        .provider_order
                        .iter()
                        .position(|entry| *entry == provider)
                    else {
                        return;
                    };
                    settings.ai.move_provider(from, to);
                });
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(
                        div()
                            .id(SharedString::from(format!(
                                "drag-ai-provider-{}",
                                provider.as_str()
                            )))
                            .flex_none()
                            .cursor_grab()
                            .on_drag(drag, move |_, _, _, cx| {
                                provider_drag_view.update(cx, |this, cx| {
                                    if this.active_model_drag.take().is_some() {
                                        cx.notify();
                                    }
                                });
                                cx.new(|_| drag_preview.clone())
                            })
                            .child(
                                Icon::new(IconName::GripVertical)
                                    .small()
                                    .text_color(theme.muted_foreground),
                            ),
                    )
                    .child(provider_name),
            )
            .child(
                Switch::new(SharedString::from(format!(
                    "toggle-ai-provider-{}",
                    provider.as_str()
                )))
                .small()
                .checked(enabled)
                .on_click(move |enabled, _, cx| {
                    SettingsWindow::update_settings(&toggle_view, cx, {
                        let enabled = *enabled;
                        move |settings| {
                            settings.ai.set_provider_enabled(provider, enabled);
                        }
                    });
                }),
            )
            .into_any_element()
    }

    fn confirm_delete_custom_provider(
        &mut self,
        provider: AiProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !provider.is_custom() {
            return;
        }
        let name = cx
            .global::<SettingsStore>()
            .settings
            .ai
            .provider_name(provider);
        let confirmation = window.prompt(
            PromptLevel::Warning,
            &format!("Delete {name}?"),
            Some("This removes its API credentials and model list."),
            &[
                PromptButton::ok("Delete provider"),
                PromptButton::cancel(crate::fl!("cancel")),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if confirmation.await != Ok(0) {
                return;
            }
            _ = this.update_in(cx, |this, _, cx| {
                let mut settings = cx.global::<SettingsStore>().settings.clone();
                settings.ai.remove_custom_provider(provider);
                if this
                    .provider_rename
                    .as_ref()
                    .is_some_and(|rename| rename.provider == provider)
                {
                    this.provider_rename = None;
                }
                this.invalidate_provider_operation(provider);
                this.model_catalogs.remove(&provider);
                this.persist_settings(settings, cx);
            });
        })
        .detach();
    }

    fn model_setting_item(
        &self,
        title: impl Into<SharedString>,
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
    }

    pub(super) fn ai_settings_page(&self, cx: &mut Context<Self>) -> SettingPage {
        let view = cx.entity();
        let defaults = Settings::default();

        let main_model =
            self.model_setting_item(crate::fl!("settings-main-model"), ModelTarget::Main, cx);
        let title_model =
            self.model_setting_item(crate::fl!("settings-title-model"), ModelTarget::Title, cx);
        let translation_model = self.model_setting_item(
            crate::fl!("settings-translation-model"),
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
        );

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
        );

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
        );

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
        );

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

        let quick_position = SettingItem::new(
            crate::fl!("settings-quick-ask-position"),
            SettingField::dropdown(
                QuickAskPosition::ALL
                    .into_iter()
                    .map(|position| Self::setting_option(position.as_str(), position.label()))
                    .collect(),
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .ai
                        .quick_ask
                        .position
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let Some(position) = QuickAskPosition::from_str(&value) else {
                            return;
                        };
                        Self::update_settings(&view, cx, move |settings| {
                            settings.ai.quick_ask.position = position;
                        });
                    }
                },
            )
            .default_value(defaults.ai.quick_ask.position.as_str()),
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

        SettingPage::new("AI").groups([
            SettingGroup::new()
                .title(crate::fl!("settings-default-models"))
                .items([main_model, title_model, translation_model, thinking]),
            SettingGroup::new()
                .title(crate::fl!("settings-page-context"))
                .items([text_format, image_quality, page_image_limit]),
            SettingGroup::new()
                .title(crate::fl!("settings-quick-ai-chat"))
                .items([quick_position, quick_transparency, quick_text_size]),
        ])
    }

    pub(super) fn provider_settings_page(&self, cx: &mut Context<Self>) -> SettingPage {
        let view = cx.entity();
        let provider_list_view = view.clone();
        let add_provider_view = view.clone();
        let provider_list = SettingItem::render(move |_, _, cx| {
            let providers = {
                let settings = &cx.global::<SettingsStore>().settings.ai;
                settings
                    .ordered_providers()
                    .iter()
                    .map(|provider| {
                        let config = settings.provider_config(*provider);
                        (*provider, config.name, config.enabled)
                    })
                    .collect::<Vec<_>>()
            };
            let rows = providers
                .into_iter()
                .map(|(provider, name, enabled)| {
                    Self::provider_order_item(
                        provider,
                        name,
                        enabled,
                        provider_list_view.clone(),
                        cx,
                    )
                })
                .collect::<Vec<_>>();
            let add_provider_view = add_provider_view.clone();
            v_flex().w_full().gap_2().children(rows).child(
                h_flex()
                    .id("add-custom-ai-provider")
                    .w_full()
                    .justify_center()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .border_1()
                    .border_dashed()
                    .border_color(cx.theme().border)
                    .rounded(px(6.0))
                    .text_color(cx.theme().muted_foreground)
                    .cursor_pointer()
                    .hover(|style| {
                        style
                            .border_color(cx.theme().primary)
                            .text_color(cx.theme().primary)
                    })
                    .on_click(move |_, window, cx| {
                        AddProviderDialog::open(add_provider_view.clone(), window, cx);
                    })
                    .child("+")
                    .child("Add model provider"),
            )
        });

        let mut groups = vec![
            SettingGroup::new()
                .title("Model providers")
                .items([provider_list]),
        ];
        let provider_order = cx
            .global::<SettingsStore>()
            .settings
            .ai
            .provider_order
            .clone();
        for provider in provider_order {
            let provider_config = cx
                .global::<SettingsStore>()
                .settings
                .ai
                .provider_config(provider);
            let provider_name = provider_config.name.clone();
            if provider == AiProvider::ChatGptSubscription {
                let account = SettingItem::new(
                    crate::fl!("settings-account"),
                    SettingField::render(move |options, window, cx| {
                        let auth = ChatGptSubscription::global(cx);
                        let snapshot = auth.as_ref().map(ChatGptSubscription::snapshot);
                        let loading = snapshot.as_ref().is_some_and(|snapshot| snapshot.loading);
                        let signing_in = snapshot
                            .as_ref()
                            .is_some_and(|snapshot| snapshot.signing_in);
                        let email = snapshot
                            .as_ref()
                            .and_then(|snapshot| snapshot.email.clone());
                        let error = snapshot.and_then(|snapshot| snapshot.error);
                        let signed_in = email.is_some();
                        let hover_state = window.use_keyed_state(
                            "chatgpt-subscription-account-hover",
                            cx,
                            |_, _| false,
                        );
                        let account_hovered = *hover_state.read(cx);
                        let hover_state_update = hover_state.clone();
                        let sign_out_label: SharedString = crate::fl!("settings-sign-out").into();
                        let button_label: SharedString = if loading {
                            crate::fl!("settings-loading-account").into()
                        } else if signing_in {
                            crate::fl!("settings-signing-in").into()
                        } else if signed_in && account_hovered {
                            sign_out_label.clone()
                        } else if let Some(email) = email.as_ref() {
                            email.clone().into()
                        } else {
                            crate::fl!("settings-sign-in").into()
                        };
                        let sizing_email = email.map(SharedString::from);
                        let visible_button_label = button_label.clone();
                        let auth_button = Button::new("chatgpt-subscription-auth")
                            .debug_selector(|| "chatgpt-account-button".to_string())
                            .when_some(sizing_email, move |button, email| {
                                button.child(stable_account_button_content(
                                    email,
                                    sign_out_label,
                                    visible_button_label,
                                ))
                            })
                            .when(!signed_in, |button| button.label(button_label))
                            .when(
                                signed_in && account_hovered && !loading && !signing_in,
                                |button| button.danger(),
                            )
                            .outline()
                            .with_size(options.size)
                            .loading(loading || signing_in)
                            .disabled(loading || signing_in || auth.is_none())
                            .on_hover(move |hovered, _, cx| {
                                if !signed_in {
                                    return;
                                }
                                hover_state_update.update(cx, |current, cx| {
                                    if *current != *hovered {
                                        *current = *hovered;
                                        cx.notify();
                                    }
                                });
                            })
                            .on_click(move |_, _, cx| {
                                let Some(auth) = ChatGptSubscription::global(cx) else {
                                    return;
                                };
                                if auth.is_authenticated() {
                                    auth.sign_out(cx);
                                } else {
                                    auth.sign_in(cx);
                                }
                            });

                        v_flex().items_end().gap_1().child(auth_button).when_some(
                            error,
                            |this, error| {
                                this.child(
                                    Label::new(error).text_xs().text_color(cx.theme().danger),
                                )
                            },
                        )
                    }),
                );

                groups.push(SettingGroup::new().title(provider_name).items([
                    account,
                    Self::provider_divider(provider, "account"),
                    Self::provider_models_item(provider, view.clone()),
                ]));
                continue;
            }

            let mut items = Vec::new();
            if provider.is_custom() {
                let compatibility = match provider_config.api_style {
                    AiApiStyle::Anthropic => "Anthropic Compatible",
                    AiApiStyle::OpenAi | AiApiStyle::OpenAiResponses => "OpenAI Compatible",
                };
                items.push(SettingItem::new(
                    "Compatibility",
                    SettingField::render(move |_, _, _| Label::new(compatibility).text_sm()),
                ));
                items.push(Self::provider_divider(provider, "compatibility"));
                items.push(Self::provider_api_host_item(provider, view.clone()));
            }

            items.push(Self::provider_api_key_item(provider, view.clone()));
            items.push(Self::provider_connection_item(provider, view.clone()));
            items.push(Self::provider_divider(provider, "credentials"));
            items.push(Self::provider_models_item(provider, view.clone()));

            if provider.is_custom() {
                items.push(Self::provider_divider(provider, "actions"));
                let delete_view = view.clone();
                items.push(SettingItem::render(move |_, _, _| {
                    let delete_view = delete_view.clone();
                    h_flex().w_full().justify_end().child(
                        Button::new(SharedString::from(format!(
                            "delete-provider-{}",
                            provider.as_str()
                        )))
                        .label("Delete provider")
                        .danger()
                        .outline()
                        .on_click(move |_, window, cx| {
                            delete_view.update(cx, |this, cx| {
                                this.confirm_delete_custom_provider(provider, window, cx);
                            });
                        }),
                    )
                }));
            }

            groups.push(SettingGroup::new().title(provider_name).items(items));
        }

        SettingPage::new(crate::fl!("settings-tab-model-providers")).groups(groups)
    }
}

#[cfg(test)]
mod tests {
    use super::{normalized_provider_name, stable_account_button_content};
    use glyphst_settings::{SettingsStore, sync_component_theme};
    use gpui::{Context, IntoElement, Render, TestAppContext, Window, prelude::*};
    use gpui_component::{
        button::{Button, ButtonVariants as _},
        v_flex,
    };

    #[test]
    fn provider_rename_trims_nonempty_names_and_rejects_blank_drafts() {
        assert_eq!(
            normalized_provider_name("  Private Models  ").as_deref(),
            Some("Private Models")
        );
        assert_eq!(normalized_provider_name(" \t\n"), None);
    }

    struct AccountButtonSizingFixture;

    impl Render for AccountButtonSizingFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let email = "mr.example@gmail.com";
            let sign_out = "Sign out";
            v_flex()
                .child(
                    Button::new("account-email-state")
                        .debug_selector(|| "account-email-state".to_string())
                        .outline()
                        .child(stable_account_button_content(
                            email.into(),
                            sign_out.into(),
                            email.into(),
                        )),
                )
                .child(
                    Button::new("account-sign-out-state")
                        .debug_selector(|| "account-sign-out-state".to_string())
                        .danger()
                        .outline()
                        .child(stable_account_button_content(
                            email.into(),
                            sign_out.into(),
                            sign_out.into(),
                        )),
                )
        }
    }

    #[gpui::test]
    fn account_button_keeps_the_same_bounds_when_its_label_changes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            let store = SettingsStore::memory();
            store.install_theme_catalog(cx).unwrap();
            cx.set_global(store);
            sync_component_theme(cx);
        });
        let (_, cx) = cx.add_window_view(|_, _| AccountButtonSizingFixture);
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let email = cx.debug_bounds("account-email-state").unwrap();
        let sign_out = cx.debug_bounds("account-sign-out-state").unwrap();
        assert_eq!(email.size, sign_out.size);
    }
}
