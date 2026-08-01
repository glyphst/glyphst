use super::super::SettingsWindow;
use glyphst_core::EpubTheme;
use glyphst_settings::{
    AppLanguage, CopyFormat, KeymapPreset, Settings, SettingsStore, TabBarVisibility,
    WindowBackgroundMode,
};
use gpui::{App, Context, SharedString};
use gpui_component::{
    Sizable as _,
    button::Button,
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage},
};

impl SettingsWindow {
    pub(super) fn general_settings_page(&self, cx: &mut Context<Self>) -> SettingPage {
        let view = cx.entity();
        let defaults = Settings::default();

        let export_settings = SettingItem::new(
            crate::fl!("settings-export"),
            SettingField::render({
                let view = view.clone();
                move |options, _, _| {
                    let view = view.clone();
                    Button::new("settings-export")
                        .label(crate::fl!("export"))
                        .outline()
                        .with_size(options.size)
                        .on_click(move |_, window, cx| {
                            view.update(cx, |this, cx| this.export(window, cx));
                        })
                }
            }),
        );

        let language = SettingItem::new(
            crate::fl!("settings-language"),
            SettingField::dropdown(
                [
                    AppLanguage::System,
                    AppLanguage::English,
                    AppLanguage::SimplifiedChinese,
                ]
                .into_iter()
                .map(|language| Self::setting_option(language.as_str(), language.label()))
                .collect(),
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .language
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let language = match value.as_ref() {
                            "en" => AppLanguage::English,
                            "zh-CN" => AppLanguage::SimplifiedChinese,
                            _ => AppLanguage::System,
                        };
                        Self::update_settings(&view, cx, move |settings| {
                            settings.language = language;
                        });
                    }
                },
            )
            .default_value(defaults.language.as_str().to_string()),
        );

        let default_zoom = SettingItem::new(
            "Default zoom",
            SettingField::number_input(
                NumberFieldOptions {
                    min: 0.25,
                    max: 8.0,
                    step: 0.05,
                },
                |cx: &App| cx.global::<SettingsStore>().settings.behavior.default_zoom as f64,
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.behavior.default_zoom = value as f32;
                        });
                    }
                },
            )
            .default_value(defaults.behavior.default_zoom as f64),
        );

        let zoom_step = SettingItem::new(
            "Zoom step",
            SettingField::number_input(
                NumberFieldOptions {
                    min: 1.01,
                    max: 4.0,
                    step: 0.05,
                },
                |cx: &App| cx.global::<SettingsStore>().settings.behavior.zoom_step as f64,
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.behavior.zoom_step = value as f32;
                        });
                    }
                },
            )
            .default_value(defaults.behavior.zoom_step as f64),
        );

        let smooth_scrolling = SettingItem::new(
            "Smooth scrolling",
            SettingField::switch(
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .behavior
                        .smooth_scrolling
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.behavior.smooth_scrolling = value;
                        });
                    }
                },
            )
            .default_value(defaults.behavior.smooth_scrolling),
        );

        let reduce_motion = SettingItem::new(
            "Reduce motion",
            SettingField::switch(
                |cx: &App| cx.global::<SettingsStore>().settings.behavior.reduce_motion,
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.behavior.reduce_motion = value;
                        });
                    }
                },
            )
            .default_value(defaults.behavior.reduce_motion),
        );

        let dim_inactive = SettingItem::new(
            "Dim inactive panes",
            SettingField::switch(
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .behavior
                        .dim_inactive_panes
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.behavior.dim_inactive_panes = value;
                        });
                    }
                },
            )
            .default_value(defaults.behavior.dim_inactive_panes),
        );

        let copy_format = SettingItem::new(
            crate::fl!("settings-copy-format"),
            SettingField::dropdown(
                vec![
                    Self::setting_option(
                        CopyFormat::PlainText.as_str(),
                        crate::fl!("settings-copy-plain-text"),
                    ),
                    Self::setting_option(
                        CopyFormat::Markdown.as_str(),
                        crate::fl!("settings-copy-markdown"),
                    ),
                ],
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .behavior
                        .copy_format
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let format = if value.as_ref() == "markdown" {
                            CopyFormat::Markdown
                        } else {
                            CopyFormat::PlainText
                        };
                        Self::update_settings(&view, cx, move |settings| {
                            settings.behavior.copy_format = format;
                        });
                    }
                },
            )
            .default_value(defaults.behavior.copy_format.as_str().to_string()),
        );

        let semantic_copy = SettingItem::new(
            crate::fl!("settings-copy-semantic"),
            SettingField::switch(
                |cx: &App| cx.global::<SettingsStore>().settings.behavior.semantic_copy,
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.behavior.semantic_copy = value;
                        });
                    }
                },
            )
            .default_value(defaults.behavior.semantic_copy),
        );

        let reading_font_size = SettingItem::new(
            crate::fl!("settings-reading-font-size"),
            SettingField::number_input(
                NumberFieldOptions {
                    min: 8.0,
                    max: 48.0,
                    step: 1.0,
                },
                |cx: &App| cx.global::<SettingsStore>().settings.reading.font_size as f64,
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.reading.font_size = value as f32;
                        });
                    }
                },
            )
            .default_value(defaults.reading.font_size as f64),
        );

        let reading_line_height = SettingItem::new(
            crate::fl!("settings-reading-line-height"),
            SettingField::number_input(
                NumberFieldOptions {
                    min: 1.0,
                    max: 3.0,
                    step: 0.1,
                },
                |cx: &App| cx.global::<SettingsStore>().settings.reading.line_height as f64,
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.reading.line_height = value as f32;
                        });
                    }
                },
            )
            .default_value(defaults.reading.line_height as f64),
        );

        let reading_margin = SettingItem::new(
            crate::fl!("settings-reading-margin"),
            SettingField::number_input(
                NumberFieldOptions {
                    min: 0.0,
                    max: 256.0,
                    step: 4.0,
                },
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .reading
                        .horizontal_margin as f64
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.reading.horizontal_margin = value as f32;
                        });
                    }
                },
            )
            .default_value(defaults.reading.horizontal_margin as f64),
        );

        let reading_width = SettingItem::new(
            crate::fl!("settings-reading-width"),
            SettingField::number_input(
                NumberFieldOptions {
                    min: 240.0,
                    max: 2048.0,
                    step: 20.0,
                },
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .reading
                        .max_content_width as f64
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.reading.max_content_width = value as f32;
                        });
                    }
                },
            )
            .default_value(defaults.reading.max_content_width as f64),
        );

        let reading_theme = SettingItem::new(
            "Reading theme",
            SettingField::dropdown(
                [
                    Self::setting_option(EpubTheme::Light.as_str(), "Light"),
                    Self::setting_option(EpubTheme::Dark.as_str(), "Dark"),
                    Self::setting_option(EpubTheme::Sepia.as_str(), "Sepia"),
                ]
                .into(),
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .reading
                        .theme
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let theme = EpubTheme::parse(value.as_ref()).unwrap_or_default();
                        Self::update_settings(&view, cx, move |settings| {
                            settings.reading.theme = theme;
                        });
                    }
                },
            )
            .default_value(defaults.reading.theme.as_str().to_string()),
        );

        let publisher_styles = SettingItem::new(
            crate::fl!("settings-reading-publisher-styles"),
            SettingField::switch(
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .reading
                        .use_publisher_styles
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.reading.use_publisher_styles = value;
                        });
                    }
                },
            )
            .default_value(defaults.reading.use_publisher_styles),
        );

        let window_opacity = SettingItem::new(
            "Window opacity",
            SettingField::number_input(
                NumberFieldOptions {
                    min: 0.1,
                    max: 1.0,
                    step: 0.05,
                },
                |cx: &App| cx.global::<SettingsStore>().settings.window.opacity as f64,
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.window.opacity = value as f32;
                        });
                    }
                },
            )
            .default_value(defaults.window.opacity as f64),
        );

        let window_background = SettingItem::new(
            crate::fl!("settings-background"),
            SettingField::dropdown(
                vec![
                    Self::setting_option("opaque", "Opaque"),
                    Self::setting_option("transparent", "Transparent"),
                    Self::setting_option("blurred", "Blurred"),
                ],
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .window
                        .background
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let background = match value.as_ref() {
                            "opaque" => WindowBackgroundMode::Opaque,
                            "transparent" => WindowBackgroundMode::Transparent,
                            _ => WindowBackgroundMode::Blurred,
                        };
                        Self::update_settings(&view, cx, move |settings| {
                            settings.window.background = background;
                        });
                    }
                },
            )
            .default_value(defaults.window.background.as_str().to_string()),
        );

        let tab_bar = SettingItem::new(
            crate::fl!("settings-tab-bar"),
            SettingField::dropdown(
                vec![
                    Self::setting_option(TabBarVisibility::Auto.as_str(), "Auto"),
                    Self::setting_option(TabBarVisibility::Always.as_str(), "Always"),
                ],
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .tab_bar_visibility
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let visibility = if value.as_ref() == "always" {
                            TabBarVisibility::Always
                        } else {
                            TabBarVisibility::Auto
                        };
                        Self::update_settings(&view, cx, move |settings| {
                            settings.tab_bar_visibility = visibility;
                        });
                    }
                },
            )
            .default_value(defaults.tab_bar_visibility.as_str().to_string()),
        );

        SettingPage::new(crate::fl!("settings-tab-general"))
            .default_open(true)
            .groups([
                SettingGroup::new()
                    .title(crate::fl!("settings-section-language"))
                    .item(language),
                SettingGroup::new()
                    .title(crate::fl!("settings-section-zoom"))
                    .items([default_zoom, zoom_step]),
                SettingGroup::new()
                    .title(crate::fl!("settings-section-scroll"))
                    .items([smooth_scrolling, reduce_motion, dim_inactive]),
                SettingGroup::new()
                    .title(crate::fl!("settings-section-copy"))
                    .items([copy_format, semantic_copy]),
                SettingGroup::new()
                    .title(crate::fl!("settings-section-reading"))
                    .items([
                        reading_font_size,
                        reading_line_height,
                        reading_margin,
                        reading_width,
                        reading_theme,
                        publisher_styles,
                    ]),
                SettingGroup::new()
                    .title(crate::fl!("settings-section-window"))
                    .items([window_opacity, window_background]),
                SettingGroup::new()
                    .title(crate::fl!("settings-section-tabs"))
                    .item(tab_bar),
                SettingGroup::new()
                    .title(crate::fl!("settings-section-general"))
                    .item(export_settings),
            ])
    }

    pub(super) fn keymap_settings_page(&self, cx: &mut Context<Self>) -> SettingPage {
        let view = cx.entity();
        let defaults = Settings::default();
        let preset = SettingItem::new(
            crate::fl!("settings-base-preset"),
            SettingField::dropdown(
                [KeymapPreset::Modern, KeymapPreset::Vim]
                    .into_iter()
                    .map(|preset| Self::setting_option(preset.as_str(), preset.label()))
                    .collect(),
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .keymap_preset
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let preset = if value.as_ref() == "vim" {
                            KeymapPreset::Vim
                        } else {
                            KeymapPreset::Modern
                        };
                        Self::update_settings(&view, cx, move |settings| {
                            settings.keymap_preset = preset;
                        });
                    }
                },
            )
            .default_value(defaults.keymap_preset.as_str().to_string()),
        );

        let open_keymap = self.open_keymap;
        let editor = SettingItem::new(
            crate::fl!("settings-keymap-editor"),
            SettingField::render(move |options, _, _| {
                Button::new("open-keymap-editor")
                    .label(crate::fl!("settings-open-keymap"))
                    .outline()
                    .with_size(options.size)
                    .on_click(move |_, _, cx| open_keymap(cx))
            }),
        );

        SettingPage::new(crate::fl!("settings-tab-keymap")).group(
            SettingGroup::new()
                .title(crate::fl!("settings-section-keymap"))
                .items([preset, editor]),
        )
    }
}
