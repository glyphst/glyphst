use super::SettingsWindow;
use glyphst_settings::{
    AiProvider, ProviderSecretId, SettingsStore,
    theme::{PartialTheme, Theme},
};
use glyphst_ui::{
    icons::{Icon, IconName},
    tooltip::text_tooltip,
};
use gpui::{
    Anchor, AnyElement, App, AppContext, Context, Entity, Hsla, IntoElement, Render, SharedString,
    Subscription, Window, div, prelude::*, px, rgb, rgba,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt as _, Theme as ComponentTheme,
    button::{Button, ButtonVariants as _},
    color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState},
    h_flex,
    input::{Input, InputEvent, InputState},
    label::Label,
    popover::{Popover, PopoverState},
    scroll::ScrollableElement as _,
    setting::{RenderOptions, SettingField, SettingItem},
    tag::Tag,
    v_flex,
};
use std::{collections::BTreeSet, rc::Rc};

type TextChangeHandler = Rc<dyn Fn(SharedString, &mut App)>;

struct BoundInputConfig {
    id: SharedString,
    value: SharedString,
    placeholder: SharedString,
    masked: bool,
    on_change: TextChangeHandler,
}

struct BoundInputState {
    input: Entity<InputState>,
    current: SharedString,
    last_external: SharedString,
    pending_submission: bool,
    suppress_change: bool,
    _subscription: Subscription,
}

struct ModelIdInputState {
    input: Entity<InputState>,
    _subscription: Subscription,
}

struct ModelCatalogSearchState {
    input: Entity<InputState>,
    _subscription: Subscription,
}

#[derive(Clone)]
struct DraggedProviderModel {
    provider: AiProvider,
    id: String,
    name: String,
}

impl Render for DraggedProviderModel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Tag::secondary()
            .outline()
            .small()
            .rounded_full()
            .child(self.name.clone())
    }
}

struct ColorPickerBinding {
    picker: Entity<ColorPickerState>,
    current: u32,
    _subscription: Subscription,
}

#[derive(Clone, Copy)]
pub(super) struct ColorSetting {
    pub id: &'static str,
    pub label: &'static str,
    pub with_alpha: bool,
    pub override_value: fn(&PartialTheme) -> Option<u32>,
    pub resolved_value: fn(&Theme, &ComponentTheme) -> u32,
    pub set_override: fn(&mut PartialTheme, Option<u32>),
}

fn channel_to_u8(channel: f32) -> u32 {
    (channel.clamp(0.0, 1.0) * 255.0).round() as u32
}

fn fetched_model_display_name(label: &str, id: &str) -> String {
    let label = label.trim();
    if label.is_empty() {
        id.to_string()
    } else {
        label.to_string()
    }
}

pub(super) fn hsla_to_u32(color: Hsla, with_alpha: bool) -> u32 {
    let color_rgb = color.to_rgb();
    let rgb = (channel_to_u8(color_rgb.r) << 16)
        | (channel_to_u8(color_rgb.g) << 8)
        | channel_to_u8(color_rgb.b);
    if with_alpha {
        (rgb << 8) | channel_to_u8(color.a)
    } else {
        rgb
    }
}

fn u32_to_hsla(color: u32, with_alpha: bool) -> Hsla {
    if with_alpha {
        rgba(color).into()
    } else {
        rgb(color).into()
    }
}

impl SettingsWindow {
    fn bound_input(
        config: BoundInputConfig,
        options: &RenderOptions,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let BoundInputConfig {
            id,
            value,
            placeholder,
            masked,
            on_change,
        } = config;
        let selector = format!("setting-bound-input-{id}");
        let initial_value = value.clone();
        let state = window.use_keyed_state(id, cx, move |window, cx| {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(initial_value.clone())
                    .placeholder(placeholder.clone())
                    .masked(masked)
            });
            let subscription = cx.subscribe_in(
                &input,
                window,
                move |state: &mut BoundInputState, input, event: &InputEvent, _, cx| {
                    if !matches!(event, InputEvent::Change) {
                        return;
                    }
                    if state.suppress_change {
                        state.suppress_change = false;
                        return;
                    }
                    let value = input.read(cx).value();
                    state.current = value.clone();
                    state.pending_submission = true;
                    on_change(value, cx);
                },
            );
            BoundInputState {
                input,
                current: initial_value.clone(),
                last_external: initial_value,
                pending_submission: false,
                suppress_change: false,
                _subscription: subscription,
            }
        });

        state.update(cx, |state, cx| {
            if state.pending_submission {
                // The immediately following external value is the sanitized
                // form of this input's own edit. Keep the user's draft intact
                // so intermediate values such as `https://` remain editable.
                state.last_external = value.clone();
                state.pending_submission = false;
            } else if state.last_external != value {
                state.current = value.clone();
                state.last_external = value.clone();
                state.suppress_change = true;
                state.input.update(cx, |input, cx| {
                    input.set_value(value.clone(), window, cx);
                });
            }
        });

        let input = state.read(cx).input.clone();
        div()
            .debug_selector(move || selector.clone())
            .w_full()
            .min_w_0()
            .child(
                Input::new(&input)
                    .disabled(options.disabled)
                    .with_size(options.size)
                    .when(masked, |input| input.mask_toggle())
                    .w_full()
                    .min_w_0(),
            )
            .into_any_element()
    }

    pub(super) fn bound_text_field<V, S>(
        id: impl Into<SharedString>,
        placeholder: impl Into<SharedString>,
        masked: bool,
        value: V,
        set_value: S,
    ) -> SettingField<SharedString>
    where
        V: Fn(&App) -> SharedString + 'static,
        S: Fn(SharedString, &mut App) + 'static,
    {
        let id = id.into();
        let placeholder = placeholder.into();
        let value: Rc<dyn Fn(&App) -> SharedString> = Rc::new(value);
        let set_value: TextChangeHandler = Rc::new(set_value);
        SettingField::render(move |options, window, cx| {
            Self::bound_input(
                BoundInputConfig {
                    id: id.clone(),
                    value: value(cx),
                    placeholder: placeholder.clone(),
                    masked,
                    on_change: set_value.clone(),
                },
                options,
                window,
                cx,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn bound_text_input<S>(
        id: impl Into<SharedString>,
        value: impl Into<SharedString>,
        placeholder: impl Into<SharedString>,
        masked: bool,
        set_value: S,
        options: &RenderOptions,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement
    where
        S: Fn(SharedString, &mut App) + 'static,
    {
        Self::bound_input(
            BoundInputConfig {
                id: id.into(),
                value: value.into(),
                placeholder: placeholder.into(),
                masked,
                on_change: Rc::new(set_value),
            },
            options,
            window,
            cx,
        )
    }

    pub(super) fn provider_connection_field(
        provider: AiProvider,
        view: Entity<Self>,
        options: &RenderOptions,
        cx: &mut App,
    ) -> AnyElement {
        let credentials_loading = cx
            .global::<SettingsStore>()
            .provider_secret_is_loading(ProviderSecretId::AiApiKey(provider));
        let (connection_loading, connection_status, connection_failed) = {
            let connection = &view.read(cx).provider_operation;
            if connection.provider == Some(provider) {
                (
                    connection.loading,
                    connection.status.clone(),
                    connection.failed,
                )
            } else {
                (false, None, false)
            }
        };
        let test_view = view;
        let muted = cx.theme().muted_foreground;

        h_flex()
            .w_full()
            .min_w_0()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .min_w_0()
                    .when_some(connection_status, |this, status| {
                        this.child(
                            Label::new(status)
                                .text_xs()
                                .text_color(if connection_failed {
                                    cx.theme().danger
                                } else {
                                    muted
                                }),
                        )
                    }),
            )
            .child(
                Button::new(SharedString::from(format!(
                    "test-provider-{}",
                    provider.as_str()
                )))
                .label(crate::fl!("settings-test-connection"))
                .outline()
                .loading(connection_loading || credentials_loading)
                .with_size(options.size)
                .disabled(options.disabled || connection_loading || credentials_loading)
                .on_click(move |_, _, cx| {
                    test_view.update(cx, |this, cx| {
                        this.test_provider_connection(provider, cx);
                    });
                }),
            )
            .into_any_element()
    }

    fn stage_manual_model_input(
        provider: AiProvider,
        view: &Entity<Self>,
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let model_id = input.read(cx).value().trim().to_string();
        if model_id.is_empty()
            || cx
                .global::<SettingsStore>()
                .settings
                .ai
                .provider_config(provider)
                .model(&model_id)
                .is_some()
        {
            return;
        }
        let staged = view.update(cx, |this, cx| {
            let staged = this.stage_manual_model_id(provider, model_id);
            if staged {
                cx.notify();
            }
            staged
        });
        if staged {
            input.update(cx, |input, cx| {
                input.set_value(SharedString::default(), window, cx);
            });
        }
    }

    fn provider_model_picker_panel(
        provider: AiProvider,
        view: Entity<Self>,
        manual_input: Entity<InputState>,
        search_input: Entity<InputState>,
        popover: Entity<PopoverState>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let (
            catalog_loading,
            catalog_status,
            catalog_failed,
            catalog_cached,
            catalog,
            selected,
            manual,
        ) = {
            let this = view.read(cx);
            let operation = &this.model_catalog_operation;
            let cached = this.model_catalogs.get(&provider);
            let (loading, status, failed) = if operation.provider == Some(provider) {
                (
                    operation.loading,
                    operation.status.clone(),
                    operation.failed,
                )
            } else {
                (false, None, false)
            };
            let (selected, manual) = if this.model_picker.provider == Some(provider) {
                (
                    this.model_picker.selected.clone(),
                    this.model_picker.manual.clone(),
                )
            } else {
                Default::default()
            };
            (
                loading,
                status,
                failed,
                cached.is_some(),
                cached
                    .map(|catalog| catalog.models.clone())
                    .unwrap_or_default(),
                selected,
                manual,
            )
        };
        let existing = cx
            .global::<SettingsStore>()
            .settings
            .ai
            .provider_config(provider)
            .models
            .into_iter()
            .map(|model| model.id)
            .collect::<std::collections::BTreeSet<_>>();
        let query = search_input.read(cx).value().trim().to_ascii_lowercase();
        let filtered_models = catalog
            .iter()
            .filter(|model| {
                query.is_empty()
                    || model.id.to_ascii_lowercase().contains(&query)
                    || model.label.to_ascii_lowercase().contains(&query)
                    || model
                        .owned_by
                        .as_deref()
                        .is_some_and(|owner| owner.to_ascii_lowercase().contains(&query))
            })
            .cloned()
            .collect::<Vec<_>>();
        let has_filtered_models = !filtered_models.is_empty();
        let visible_selectable_ids = filtered_models
            .iter()
            .filter(|model| !existing.contains(&model.id))
            .map(|model| model.id.clone())
            .collect::<Vec<_>>();
        let pending = selected.union(&manual).cloned().collect::<BTreeSet<_>>();
        let selected_count = pending
            .iter()
            .filter(|model_id| {
                !existing.contains(*model_id)
                    && (manual.contains(*model_id)
                        || catalog.iter().any(|model| model.id == model_id.as_str()))
            })
            .count();
        let manual_value = manual_input.read(cx).value().trim().to_string();
        let can_stage_manual = !manual_value.is_empty()
            && !existing.contains(&manual_value)
            && !pending.contains(&manual_value);
        let muted = cx.theme().muted_foreground;
        let provider_name = cx
            .global::<SettingsStore>()
            .settings
            .ai
            .provider_name(provider);

        let model_tags = filtered_models.into_iter().map(|model| {
            let model_id = model.id.clone();
            let model_selector = format!(
                "provider-model-catalog-option-{}-{model_id}",
                provider.as_str()
            );
            let is_added = existing.contains(&model_id);
            let is_selected = pending.contains(&model_id);
            let label = fetched_model_display_name(&model.label, &model.id);
            if is_added {
                div()
                    .debug_selector(move || model_selector.clone())
                    .child(
                        Tag::success()
                            .outline()
                            .small()
                            .rounded_full()
                            .child(format!(
                                "{label} · {}",
                                crate::fl!("settings-model-already-added")
                            )),
                    )
                    .into_any_element()
            } else {
                let view = view.clone();
                div()
                    .id(SharedString::from(format!(
                        "provider-model-option-{}-{model_id}",
                        provider.as_str()
                    )))
                    .debug_selector(move || model_selector.clone())
                    .cursor_pointer()
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.toggle_model_picker_model(model_id.clone());
                            cx.notify();
                        });
                    })
                    .child(if is_selected {
                        Tag::primary()
                            .small()
                            .rounded_full()
                            .child(format!("✓ {label}"))
                    } else {
                        Tag::secondary()
                            .outline()
                            .small()
                            .rounded_full()
                            .child(label)
                    })
                    .into_any_element()
            }
        });

        let manual_tags = manual
            .iter()
            .cloned()
            .map(|model_id| {
                let remove_view = view.clone();
                let remove_id = model_id.clone();
                let selector = format!("provider-manual-model-{}-{model_id}", provider.as_str());
                div()
                    .id(SharedString::from(format!(
                        "remove-provider-manual-model-{}-{model_id}",
                        provider.as_str()
                    )))
                    .debug_selector(move || selector.clone())
                    .cursor_pointer()
                    .on_click(move |_, _, cx| {
                        remove_view.update(cx, |this, cx| {
                            this.remove_manual_model_id(&remove_id);
                            cx.notify();
                        });
                    })
                    .child(
                        Tag::primary()
                            .outline()
                            .small()
                            .rounded_full()
                            .child(format!("{model_id} ×")),
                    )
            })
            .collect::<Vec<_>>();

        let refresh_view = view.clone();
        let confirm_view = view.clone();
        let select_all_view = view.clone();
        let clear_view = view.clone();
        let select_all_ids = visible_selectable_ids.clone();
        let cancel_popover = popover.clone();
        let confirm_popover = popover;
        let manual_add_view = view.clone();
        let manual_add_input = manual_input.clone();
        let viewport = window.viewport_size();
        let panel_width = px(480.0).min((viewport.width - px(32.0)).max(px(0.0)));
        let panel_height = px(520.0).min((viewport.height - px(32.0)).max(px(0.0)));

        v_flex()
            .debug_selector(|| "provider-model-picker-panel".to_string())
            .w(panel_width)
            .max_w(panel_width)
            .h(panel_height)
            .max_h(panel_height)
            .overflow_hidden()
            .child(
                v_flex().flex_none().gap_1().px_3().pt_3().child(
                    Label::new(crate::fl!(
                        "settings-add-provider-models",
                        provider = provider_name
                    ))
                    .text_sm()
                    .font_semibold(),
                ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .px_3()
                    .pt_3()
                    .child(Input::new(&manual_input).small().flex_1().min_w_0())
                    .child(
                        Button::new(SharedString::from(format!(
                            "stage-provider-manual-model-{}",
                            provider.as_str()
                        )))
                        .label(crate::fl!("settings-add-model-id"))
                        .outline()
                        .small()
                        .disabled(!can_stage_manual)
                        .on_click(move |_, window, cx| {
                            Self::stage_manual_model_input(
                                provider,
                                &manual_add_view,
                                &manual_add_input,
                                window,
                                cx,
                            );
                        }),
                    ),
            )
            .when(!manual.is_empty(), |this| {
                this.child(
                    h_flex()
                        .flex_none()
                        .w_full()
                        .items_start()
                        .flex_wrap()
                        .gap_2()
                        .px_3()
                        .pt_2()
                        .children(manual_tags),
                )
            })
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .min_w_0()
                    .px_3()
                    .pt_3()
                    .child(Input::new(&search_input).small().w_full().min_w_0()),
            )
            .child(
                h_flex()
                    .flex_none()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .pt_2()
                    .child(
                        Label::new(if catalog_cached {
                            crate::fl!("settings-models-cached", count = catalog.len() as i64)
                        } else {
                            crate::fl!("settings-no-cached-models")
                        })
                        .text_xs()
                        .text_color(muted),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new(SharedString::from(format!(
                                    "select-all-provider-models-{}",
                                    provider.as_str()
                                )))
                                .label(crate::fl!("settings-select-all"))
                                .ghost()
                                .xsmall()
                                .disabled(visible_selectable_ids.is_empty())
                                .on_click(move |_, _, cx| {
                                    select_all_view.update(cx, |this, cx| {
                                        this.set_visible_model_selection(
                                            select_all_ids.clone(),
                                            true,
                                        );
                                        cx.notify();
                                    });
                                }),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "clear-provider-model-selection-{}",
                                    provider.as_str()
                                )))
                                .label(crate::fl!("settings-clear-selection"))
                                .ghost()
                                .xsmall()
                                .disabled(selected_count == 0)
                                .on_click(move |_, _, cx| {
                                    clear_view.update(cx, |this, cx| {
                                        this.clear_model_picker_selection();
                                        cx.notify();
                                    });
                                }),
                            ),
                    ),
            )
            .child(
                v_flex().flex_1().min_h_0().overflow_hidden().child(
                    div()
                        .debug_selector(|| "provider-model-catalog-scroll".to_string())
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .child(
                            v_flex()
                                .id(SharedString::from(format!(
                                    "provider-model-catalog-{}",
                                    provider.as_str()
                                )))
                                .size_full()
                                .overflow_y_scrollbar()
                                .px_3()
                                .py_2()
                                .when(!catalog_cached && !catalog_loading, |this| {
                                    this.child(
                                        Label::new(crate::fl!("settings-model-catalog-empty"))
                                            .text_sm()
                                            .text_color(muted),
                                    )
                                })
                                .when(catalog_cached && catalog.is_empty(), |this| {
                                    this.child(
                                        Label::new(crate::fl!("settings-no-models-returned"))
                                            .text_sm()
                                            .text_color(muted),
                                    )
                                })
                                .when(
                                    catalog_cached && !catalog.is_empty() && !has_filtered_models,
                                    |this| {
                                        this.child(
                                            Label::new(crate::fl!(
                                                "settings-no-matching-models-period"
                                            ))
                                            .text_sm()
                                            .text_color(muted),
                                        )
                                    },
                                )
                                .child(
                                    h_flex()
                                        .items_start()
                                        .flex_none()
                                        .flex_wrap()
                                        .gap_2()
                                        .children(model_tags),
                                ),
                        ),
                ),
            )
            .when_some(catalog_status, |this, status| {
                this.child(div().flex_none().px_3().pb_2().child(
                    Label::new(status).text_xs().text_color(if catalog_failed {
                        cx.theme().danger
                    } else {
                        muted
                    }),
                ))
            })
            .child(
                h_flex()
                    .debug_selector(|| "provider-model-picker-footer".to_string())
                    .flex_none()
                    .justify_between()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .px_3()
                    .py_2()
                    .child(
                        Button::new(SharedString::from(format!(
                            "refresh-provider-models-{}",
                            provider.as_str()
                        )))
                        .label(if catalog_cached {
                            crate::fl!("settings-refresh-models")
                        } else {
                            crate::fl!("settings-fetch-models")
                        })
                        .outline()
                        .small()
                        .loading(catalog_loading)
                        .disabled(catalog_loading)
                        .on_click(move |_, _, cx| {
                            refresh_view.update(cx, |this, cx| {
                                this.fetch_provider_models(provider, cx);
                            });
                        }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new(SharedString::from(format!(
                                    "cancel-provider-model-picker-{}",
                                    provider.as_str()
                                )))
                                .label(crate::fl!("cancel"))
                                .ghost()
                                .small()
                                .on_click(
                                    move |_, window, cx| {
                                        cancel_popover.update(cx, |popover, cx| {
                                            popover.dismiss(window, cx);
                                        });
                                    },
                                ),
                            )
                            .child(
                                Button::new(SharedString::from(format!(
                                    "confirm-provider-models-{}",
                                    provider.as_str()
                                )))
                                .label(crate::fl!(
                                    "settings-add-selected-models",
                                    count = selected_count as i64
                                ))
                                .primary()
                                .small()
                                .disabled(selected_count == 0 || catalog_loading)
                                .on_click(
                                    move |_, window, cx| {
                                        confirm_view.update(cx, |this, cx| {
                                            this.add_selected_models(provider, cx);
                                        });
                                        confirm_popover.update(cx, |popover, cx| {
                                            popover.dismiss(window, cx);
                                        });
                                    },
                                ),
                            ),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn provider_model_list_field(
        provider: AiProvider,
        view: Entity<Self>,
        options: &RenderOptions,
        cx: &mut App,
    ) -> AnyElement {
        let config = cx
            .global::<SettingsStore>()
            .settings
            .ai
            .provider_config(provider);
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let danger = cx.theme().danger;
        let active_model_drag =
            view.read(cx).active_model_drag == Some(provider) && cx.has_active_drag();
        let disabled = options.disabled;
        let models_empty = config.models.is_empty();
        let model_chips = config.models.into_iter().map(|model| {
            let model_id = model.id.clone();
            let model_name = model.name.clone();
            let target_id = model.id.clone();
            let drop_target_id = model.id.clone();
            let drop_view = view.clone();
            let drag_start_view = view.clone();
            let drag = DraggedProviderModel {
                provider,
                id: model.id.clone(),
                name: model.name.clone(),
            };
            let drag_preview = drag.clone();
            div()
                .id(SharedString::from(format!(
                    "provider-model-chip-{}-{model_id}",
                    provider.as_str()
                )))
                .debug_selector({
                    let selector = format!("provider-model-chip-{}-{model_id}", provider.as_str());
                    move || selector.clone()
                })
                .when(!disabled, |this| this.cursor_grab())
                .tooltip(text_tooltip(model.id))
                .when(!disabled, move |this| {
                    this.drag_over::<DraggedProviderModel>(move |style, dragged, _, _| {
                        if dragged.provider == provider && dragged.id != target_id {
                            style.border_b(px(2.0)).border_color(primary)
                        } else {
                            style
                        }
                    })
                    .on_drop(move |dragged: &DraggedProviderModel, _, cx| {
                        if dragged.provider != provider {
                            return;
                        }
                        drop_view.update(cx, |this, cx| {
                            this.active_model_drag = None;
                            cx.notify();
                        });
                        let dragged_id = dragged.id.clone();
                        let target_id = drop_target_id.clone();
                        Self::update_settings(&drop_view, cx, move |settings| {
                            let config = settings.ai.provider_config_mut(provider);
                            let Some(from) = config
                                .models
                                .iter()
                                .position(|model| model.id == dragged_id)
                            else {
                                return;
                            };
                            let Some(to) =
                                config.models.iter().position(|model| model.id == target_id)
                            else {
                                return;
                            };
                            config.move_model(from, to);
                        });
                    })
                    .on_drag(drag, move |_, _, _, cx| {
                        drag_start_view.update(cx, |this, cx| {
                            this.active_model_drag = Some(provider);
                            cx.notify();
                        });
                        cx.new(|_| drag_preview.clone())
                    })
                })
                .child(
                    Tag::secondary()
                        .outline()
                        .small()
                        .rounded_full()
                        .child(model_name),
                )
        });

        v_flex()
            .w_full()
            .gap_2()
            .when(models_empty, |this| {
                this.child(
                    Label::new(crate::fl!("settings-no-models-added"))
                        .text_sm()
                        .text_color(muted),
                )
            })
            .child(
                h_flex()
                    .debug_selector({
                        let selector = format!("provider-model-chips-{}", provider.as_str());
                        move || selector.clone()
                    })
                    .w_full()
                    .items_start()
                    .flex_wrap()
                    .gap_2()
                    .children(model_chips),
            )
            .when(!disabled && active_model_drag, |this| {
                let trash_view = view.clone();
                this.child(
                    h_flex()
                        .id(SharedString::from(format!(
                            "delete-provider-model-drop-target-{}",
                            provider.as_str()
                        )))
                        .debug_selector({
                            let selector = format!("provider-model-trash-{}", provider.as_str());
                            move || selector.clone()
                        })
                        .h(px(48.0))
                        .w_full()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .rounded(px(6.0))
                        .border_1()
                        .border_dashed()
                        .border_color(danger.opacity(0.5))
                        .text_color(danger)
                        .occlude()
                        .drag_over::<DraggedProviderModel>(move |style, dragged, _, _| {
                            if dragged.provider == provider {
                                style.border_color(danger).bg(danger.opacity(0.08))
                            } else {
                                style
                            }
                        })
                        .on_drop(move |dragged: &DraggedProviderModel, _, cx| {
                            if dragged.provider != provider {
                                return;
                            }
                            cx.stop_propagation();
                            let model_id = dragged.id.clone();
                            trash_view.update(cx, |this, cx| {
                                this.active_model_drag = None;
                                cx.notify();
                            });
                            Self::update_settings(&trash_view, cx, move |settings| {
                                settings
                                    .ai
                                    .provider_config_mut(provider)
                                    .remove_model(&model_id);
                            });
                        })
                        .child(Icon::new(IconName::Trash2).small())
                        .child(Label::new(crate::fl!("settings-drop-model-to-delete")).text_sm()),
                )
            })
            .into_any_element()
    }

    pub(super) fn provider_add_models_button(
        provider: AiProvider,
        view: Entity<Self>,
        options: &RenderOptions,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let state = window.use_keyed_state(
            SharedString::from(format!("provider-model-input-{}", provider.as_str())),
            cx,
            {
                let view = view.clone();
                move |window, cx| {
                    let input = cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder(crate::fl!("settings-model-id-placeholder"))
                    });
                    let subscription = cx.subscribe_in(
                        &input,
                        window,
                        move |state: &mut ModelIdInputState, _, event: &InputEvent, window, cx| {
                            if matches!(event, InputEvent::Change) {
                                view.update(cx, |_, cx| cx.notify());
                            }
                            if matches!(event, InputEvent::PressEnter { .. }) {
                                let input = state.input.clone();
                                Self::stage_manual_model_input(provider, &view, &input, window, cx);
                            }
                        },
                    );
                    ModelIdInputState {
                        input,
                        _subscription: subscription,
                    }
                }
            },
        );
        let input = state.read(cx).input.clone();
        let search_state = window.use_keyed_state(
            SharedString::from(format!("provider-model-search-{}", provider.as_str())),
            cx,
            {
                let view = view.clone();
                move |window, cx| {
                    let input = cx.new(|cx| {
                        InputState::new(window, cx)
                            .placeholder(crate::fl!("settings-search-model-placeholder"))
                    });
                    let subscription = cx.subscribe_in(
                        &input,
                        window,
                        move |_: &mut ModelCatalogSearchState, _, event: &InputEvent, _, cx| {
                            if matches!(event, InputEvent::Change) {
                                view.update(cx, |_, cx| cx.notify());
                            }
                        },
                    );
                    ModelCatalogSearchState {
                        input,
                        _subscription: subscription,
                    }
                }
            },
        );
        let search_input = search_state.read(cx).input.clone();
        let picker_open = view.read(cx).model_picker.provider == Some(provider);
        let picker_view = view.clone();
        let picker_panel_view = view.clone();
        let picker_manual = input;
        let picker_search = search_input;

        if options.disabled {
            Button::new(SharedString::from(format!(
                "open-provider-model-picker-{}",
                provider.as_str()
            )))
            .label(crate::fl!("settings-add-models"))
            .outline()
            .with_size(options.size)
            .disabled(true)
            .into_any_element()
        } else {
            Popover::new(SharedString::from(format!(
                "provider-model-picker-{}",
                provider.as_str()
            )))
            .anchor(Anchor::TopRight)
            .p_0()
            .open(picker_open)
            .on_open_change(move |open, _, cx| {
                picker_view.update(cx, |this, cx| {
                    this.set_model_picker_open(provider, *open, cx);
                });
            })
            .trigger(
                Button::new(SharedString::from(format!(
                    "open-provider-model-picker-{}",
                    provider.as_str()
                )))
                .debug_selector({
                    let selector = format!("provider-add-models-{}", provider.as_str());
                    move || selector.clone()
                })
                .label(crate::fl!("settings-add-models"))
                .outline()
                .with_size(options.size),
            )
            .content(move |_, window, cx| {
                Self::provider_model_picker_panel(
                    provider,
                    picker_panel_view.clone(),
                    picker_manual.clone(),
                    picker_search.clone(),
                    cx.entity(),
                    window,
                    cx,
                )
            })
            .into_any_element()
        }
    }

    pub(super) fn color_setting_item(setting: ColorSetting, view: Entity<Self>) -> SettingItem {
        let field = SettingField::render({
            let view = view.clone();
            move |options, window, cx| {
                let settings = &cx.global::<SettingsStore>();
                let current = (setting.override_value)(&settings.settings.overrides)
                    .unwrap_or_else(|| {
                        (setting.resolved_value)(&settings.resolved_theme, cx.theme())
                    });
                let state = window.use_keyed_state(
                    SharedString::from(format!("theme-color-{}", setting.id)),
                    cx,
                    {
                        let view = view.clone();
                        move |window, cx| {
                            let picker = cx.new(|cx| {
                                ColorPickerState::new(window, cx)
                                    .default_value(u32_to_hsla(current, setting.with_alpha))
                            });
                            let subscription = cx.subscribe_in(
                                &picker,
                                window,
                                move |binding: &mut ColorPickerBinding,
                                      _,
                                      event: &ColorPickerEvent,
                                      _,
                                      cx| {
                                    let ColorPickerEvent::Change(Some(color)) = event else {
                                        return;
                                    };
                                    let color = hsla_to_u32(*color, setting.with_alpha);
                                    binding.current = color;
                                    Self::update_settings(&view, cx, move |settings| {
                                        (setting.set_override)(
                                            &mut settings.overrides,
                                            Some(color),
                                        );
                                    });
                                },
                            );
                            ColorPickerBinding {
                                picker,
                                current,
                                _subscription: subscription,
                            }
                        }
                    },
                );

                state.update(cx, |state, cx| {
                    if state.current != current {
                        state.current = current;
                        state.picker.update(cx, |picker, cx| {
                            picker.set_value(u32_to_hsla(current, setting.with_alpha), window, cx);
                        });
                    }
                });
                let picker = state.read(cx).picker.clone();
                ColorPicker::new(&picker)
                    .anchor(Anchor::TopRight)
                    .with_size(options.size)
            }
        })
        .on_reset(
            move |cx| {
                (setting.override_value)(&cx.global::<SettingsStore>().settings.overrides).is_some()
            },
            {
                let view = view.clone();
                move |_, cx| {
                    Self::update_settings(&view, cx, move |settings| {
                        (setting.set_override)(&mut settings.overrides, None);
                    });
                }
            },
        );

        SettingItem::new(setting.label, field)
    }
}

#[cfg(test)]
mod tests {
    use super::{fetched_model_display_name, hsla_to_u32, u32_to_hsla};

    #[test]
    fn theme_color_conversion_preserves_rgb_and_rgba() {
        assert_eq!(hsla_to_u32(u32_to_hsla(0x12abef, false), false), 0x12abef);
        assert_eq!(hsla_to_u32(u32_to_hsla(0x12abef66, true), true), 0x12abef66);
    }

    #[test]
    fn fetched_model_display_prefers_name_and_falls_back_to_id() {
        assert_eq!(
            fetched_model_display_name("OpenAI: GPT-5.6 Sol", "openai/gpt-5.6-sol"),
            "OpenAI: GPT-5.6 Sol"
        );
        assert_eq!(
            fetched_model_display_name("", "openai/gpt-5.6-sol"),
            "openai/gpt-5.6-sol"
        );
    }
}
