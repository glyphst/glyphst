use super::super::{
    SettingsWindow,
    fields::{ColorSetting, hsla_to_u32},
};
use glyphst_settings::{
    Settings, SettingsStore,
    theme::{PartialTheme, Theme, ThemeAppearance, ThemeAppearanceMode},
    user_themes_dir,
};
use gpui::{App, Context, SharedString};
use gpui_component::{
    Sizable as _,
    button::Button,
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage},
};

macro_rules! document_color_setting {
    ($field:ident, $label:literal, $with_alpha:expr) => {
        ColorSetting {
            id: stringify!($field),
            label: $label,
            with_alpha: $with_alpha,
            override_value: |theme: &PartialTheme| theme.$field,
            resolved_value: |theme: &Theme, _| theme.$field,
            set_override: |theme: &mut PartialTheme, value| theme.$field = value,
        }
    };
}

macro_rules! component_color_setting {
    ($field:ident => $component_field:ident, $label:literal, $with_alpha:expr) => {
        ColorSetting {
            id: stringify!($field),
            label: $label,
            with_alpha: $with_alpha,
            override_value: |theme: &PartialTheme| theme.$field,
            resolved_value: |_, theme| hsla_to_u32(theme.$component_field, $with_alpha),
            set_override: |theme: &mut PartialTheme, value| theme.$field = value,
        }
    };
}

macro_rules! synchronized_group_color_setting {
    ($index:literal, $label:literal) => {
        ColorSetting {
            id: concat!("synchronized_group_", stringify!($index)),
            label: $label,
            with_alpha: false,
            override_value: |theme: &PartialTheme| theme.synchronized_group_colors[$index - 1],
            resolved_value: |theme: &Theme, _| theme.synchronized_group_colors[$index - 1],
            set_override: |theme: &mut PartialTheme, value| {
                theme.synchronized_group_colors[$index - 1] = value
            },
        }
    };
}

const DOCUMENT_COLORS: [ColorSetting; 9] = [
    document_color_setting!(background, "Document background", false),
    document_color_setting!(page, "Page", false),
    document_color_setting!(selection_highlight, "Text selection", true),
    document_color_setting!(highlight_yellow, "Yellow highlight", true),
    document_color_setting!(highlight_green, "Green highlight", true),
    document_color_setting!(highlight_blue, "Blue highlight", true),
    document_color_setting!(highlight_pink, "Pink highlight", true),
    document_color_setting!(highlight_orange, "Orange highlight", true),
    document_color_setting!(match_highlight, "Search match", false),
];

const CHROME_COLORS: [ColorSetting; 13] = [
    component_color_setting!(overlay => overlay, "Overlay", true),
    component_color_setting!(footer_background => status_bar, "Footer background", false),
    component_color_setting!(footer_border => status_bar_border, "Footer border", false),
    component_color_setting!(footer_text => muted_foreground, "Footer text", false),
    component_color_setting!(panel_background => sidebar, "Panel background", false),
    component_color_setting!(panel_border => sidebar_border, "Panel border", false),
    component_color_setting!(surface_background => popover, "Surface background", false),
    component_color_setting!(border => border, "Border", false),
    component_color_setting!(border_variant => input, "Subtle border", false),
    component_color_setting!(border_focused => ring, "Focused border", false),
    component_color_setting!(tab_bar_background => tab_bar, "Tab bar background", false),
    component_color_setting!(tab_active_background => tab_active, "Active tab", false),
    component_color_setting!(tab_inactive_background => tab, "Inactive tab", false),
];

const CONTROL_COLORS: [ColorSetting; 12] = [
    component_color_setting!(element_hover => list_hover, "Hovered element", false),
    component_color_setting!(element_selected => list_active, "Selected element", false),
    component_color_setting!(text => foreground, "Text", false),
    component_color_setting!(text_muted => muted_foreground, "Muted text", false),
    component_color_setting!(placeholder_text => muted_foreground, "Placeholder text", true),
    component_color_setting!(accent => primary, "Accent", false),
    component_color_setting!(selected_row => list_active, "Selected row", true),
    component_color_setting!(control_background => button, "Control background", false),
    component_color_setting!(control_border => input, "Control border", false),
    component_color_setting!(control_hover => button_hover, "Control hover", false),
    component_color_setting!(success => success, "Success", false),
    component_color_setting!(error => danger, "Error", false),
];

const SYNCHRONIZED_GROUP_COLORS: [ColorSetting; 8] = [
    synchronized_group_color_setting!(1, "Group color 1"),
    synchronized_group_color_setting!(2, "Group color 2"),
    synchronized_group_color_setting!(3, "Group color 3"),
    synchronized_group_color_setting!(4, "Group color 4"),
    synchronized_group_color_setting!(5, "Group color 5"),
    synchronized_group_color_setting!(6, "Group color 6"),
    synchronized_group_color_setting!(7, "Group color 7"),
    synchronized_group_color_setting!(8, "Group color 8"),
];

impl SettingsWindow {
    pub(super) fn appearance_settings_page(&self, cx: &mut Context<Self>) -> SettingPage {
        let view = cx.entity();
        let defaults = Settings::default();
        let metadata = cx.global::<SettingsStore>().theme_catalog.metadata();
        let light_options = metadata
            .iter()
            .filter(|theme| theme.appearance == ThemeAppearance::Light)
            .map(|theme| Self::setting_option(theme.id.clone(), theme.name.clone()))
            .collect::<Vec<_>>();
        let dark_options = metadata
            .iter()
            .filter(|theme| theme.appearance == ThemeAppearance::Dark)
            .map(|theme| Self::setting_option(theme.id.clone(), theme.name.clone()))
            .collect::<Vec<_>>();

        let mode = SettingItem::new(
            crate::fl!("settings-mode"),
            SettingField::dropdown(
                vec![
                    Self::setting_option("light", "Light"),
                    Self::setting_option("dark", "Dark"),
                    Self::setting_option("system", "System"),
                ],
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .theme
                        .mode
                        .as_str()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        let mode = match value.as_ref() {
                            "light" => ThemeAppearanceMode::Light,
                            "dark" => ThemeAppearanceMode::Dark,
                            _ => ThemeAppearanceMode::System,
                        };
                        Self::update_settings(&view, cx, move |settings| {
                            settings.theme.mode = mode;
                        });
                    }
                },
            )
            .default_value(defaults.theme.mode.as_str().to_string()),
        );

        let light_theme = SettingItem::new(
            crate::fl!("settings-light-theme"),
            SettingField::scrollable_dropdown(
                light_options,
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .theme
                        .light
                        .clone()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.theme.light = value.to_string();
                        });
                    }
                },
            )
            .default_value(defaults.theme.light.clone()),
        );

        let dark_theme = SettingItem::new(
            crate::fl!("settings-dark-theme"),
            SettingField::scrollable_dropdown(
                dark_options,
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .theme
                        .dark
                        .clone()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.theme.dark = value.to_string();
                        });
                    }
                },
            )
            .default_value(defaults.theme.dark.clone()),
        );

        let themes_directory = user_themes_dir();
        let theme_directory = SettingItem::new(
            "User theme directory",
            SettingField::render(move |options, _, _| {
                let path = themes_directory.clone();
                Button::new("open-user-theme-directory")
                    .label("Show Directory")
                    .outline()
                    .with_size(options.size())
                    .on_click(move |_, _, cx| cx.reveal_path(&path))
            }),
        );

        let font_options = self
            .font_families
            .iter()
            .cloned()
            .map(|font| Self::setting_option(font.clone(), font))
            .collect::<Vec<_>>();
        let font_family = SettingItem::new(
            crate::fl!("settings-ui-font-family"),
            SettingField::scrollable_dropdown(
                font_options,
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .font
                        .ui_font_family
                        .clone()
                        .into()
                },
                {
                    let view = view.clone();
                    move |value: SharedString, cx: &mut App| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.font.ui_font_family = value.to_string();
                        });
                    }
                },
            )
            .default_value(defaults.font.ui_font_family.clone()),
        );

        let fallback_field = Self::bound_text_field(
            "ui-font-fallbacks",
            "Noto Sans CJK SC, Source Han Sans SC",
            false,
            |cx| Self::font_fallbacks(&cx.global::<SettingsStore>().settings),
            {
                let view = view.clone();
                move |value, cx| {
                    let fallbacks = Self::parse_font_fallbacks(value.as_ref());
                    Self::update_text_settings(&view, cx, move |settings| {
                        settings.font.ui_font_fallbacks = fallbacks;
                    });
                }
            },
        )
        .on_reset(
            |cx| {
                cx.global::<SettingsStore>().settings.font.ui_font_fallbacks
                    != Settings::default().font.ui_font_fallbacks
            },
            {
                let view = view.clone();
                move |_, cx| {
                    Self::update_settings(&view, cx, |settings| {
                        settings.font.ui_font_fallbacks =
                            Settings::default().font.ui_font_fallbacks;
                    });
                }
            },
        );
        let font_fallbacks = SettingItem::new("UI font fallbacks", fallback_field);

        let font_size = SettingItem::new(
            "UI font size",
            SettingField::number_input(
                NumberFieldOptions {
                    min: 6.0,
                    max: 100.0,
                    step: 1.0,
                },
                |cx: &App| cx.global::<SettingsStore>().settings.font.ui_font_size as f64,
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.font.ui_font_size = value as f32;
                        });
                    }
                },
            )
            .default_value(defaults.font.ui_font_size as f64),
        );

        let font_weight = SettingItem::new(
            "UI font weight",
            SettingField::number_input(
                NumberFieldOptions {
                    min: 100.0,
                    max: 900.0,
                    step: 100.0,
                },
                |cx: &App| cx.global::<SettingsStore>().settings.font.ui_font_weight as f64,
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.font.ui_font_weight = value as f32;
                        });
                    }
                },
            )
            .default_value(defaults.font.ui_font_weight as f64),
        );

        let page_gap = SettingItem::new(
            "Page gap",
            SettingField::number_input(
                NumberFieldOptions {
                    min: 0.0,
                    max: 240.0,
                    step: 4.0,
                },
                |cx: &App| cx.global::<SettingsStore>().resolved_theme.page_gap as f64,
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.overrides.page_gap =
                                if value as f32 == glyphst_settings::theme::DEFAULT_PAGE_GAP {
                                    None
                                } else {
                                    Some(value as f32)
                                };
                        });
                    }
                },
            )
            .default_value(glyphst_settings::theme::DEFAULT_PAGE_GAP as f64),
        );

        let follow_app_theme = SettingItem::new(
            crate::fl!("settings-document-follow-theme"),
            SettingField::switch(
                |cx: &App| {
                    cx.global::<SettingsStore>()
                        .settings
                        .document_theme
                        .follow_app_theme
                },
                {
                    let view = view.clone();
                    move |value, cx| {
                        Self::update_settings(&view, cx, move |settings| {
                            settings.document_theme.follow_app_theme = value;
                        });
                    }
                },
            )
            .default_value(defaults.document_theme.follow_app_theme),
        )
        .description(crate::fl!("settings-document-follow-theme-description"));

        let document_colors = std::iter::once(follow_app_theme).chain(
            DOCUMENT_COLORS
                .into_iter()
                .map(|setting| Self::color_setting_item(setting, view.clone())),
        );
        let chrome_colors = CHROME_COLORS
            .into_iter()
            .map(|setting| Self::color_setting_item(setting, view.clone()));
        let control_colors = CONTROL_COLORS
            .into_iter()
            .map(|setting| Self::color_setting_item(setting, view.clone()));
        let synchronized_group_colors = SYNCHRONIZED_GROUP_COLORS
            .into_iter()
            .map(|setting| Self::color_setting_item(setting, view.clone()));

        SettingPage::new(crate::fl!("settings-tab-appearance")).groups([
            SettingGroup::new()
                .title(crate::fl!("settings-section-theme"))
                .items([mode, light_theme, dark_theme, theme_directory]),
            SettingGroup::new()
                .title(crate::fl!("settings-section-fonts"))
                .items([font_family, font_fallbacks, font_size, font_weight]),
            SettingGroup::new().title("Document layout").item(page_gap),
            SettingGroup::new()
                .title(crate::fl!("settings-section-document-colors"))
                .items(document_colors),
            SettingGroup::new()
                .title("Window chrome")
                .items(chrome_colors),
            SettingGroup::new()
                .title("Text and controls")
                .items(control_colors),
            SettingGroup::new()
                .title("Synchronized groups")
                .items(synchronized_group_colors),
        ])
    }
}
