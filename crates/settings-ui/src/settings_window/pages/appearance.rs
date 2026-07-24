use super::super::{SettingsWindow, fields::ColorSetting};
use glyphst_settings::{
    Settings, SettingsStore,
    theme::{PartialTheme, Theme, ThemeAppearance, ThemeAppearanceMode},
    user_themes_dir,
};
use gpui::{App, Axis, Context, SharedString};
use gpui_component::{
    Sizable as _,
    button::Button,
    setting::{NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage},
};

macro_rules! color_setting {
    ($field:ident, $label:literal, $with_alpha:expr) => {
        ColorSetting {
            id: stringify!($field),
            label: $label,
            with_alpha: $with_alpha,
            override_value: |theme: &PartialTheme| theme.$field,
            resolved_value: |theme: &Theme| theme.$field,
            set_override: |theme: &mut PartialTheme, value| theme.$field = value,
        }
    };
}

const DOCUMENT_COLORS: [ColorSetting; 9] = [
    color_setting!(background, "Document background", false),
    color_setting!(page, "Page", false),
    color_setting!(selection_highlight, "Text selection", true),
    color_setting!(highlight_yellow, "Yellow highlight", true),
    color_setting!(highlight_green, "Green highlight", true),
    color_setting!(highlight_blue, "Blue highlight", true),
    color_setting!(highlight_pink, "Pink highlight", true),
    color_setting!(highlight_orange, "Orange highlight", true),
    color_setting!(overlay, "Overlay", true),
];

const CHROME_COLORS: [ColorSetting; 12] = [
    color_setting!(footer_background, "Footer background", false),
    color_setting!(footer_border, "Footer border", false),
    color_setting!(footer_text, "Footer text", false),
    color_setting!(panel_background, "Panel background", false),
    color_setting!(panel_border, "Panel border", false),
    color_setting!(surface_background, "Surface background", false),
    color_setting!(border, "Border", false),
    color_setting!(border_variant, "Subtle border", false),
    color_setting!(border_focused, "Focused border", false),
    color_setting!(tab_bar_background, "Tab bar background", false),
    color_setting!(tab_active_background, "Active tab", false),
    color_setting!(tab_inactive_background, "Inactive tab", false),
];

const CONTROL_COLORS: [ColorSetting; 13] = [
    color_setting!(element_hover, "Hovered element", false),
    color_setting!(element_selected, "Selected element", false),
    color_setting!(text, "Text", false),
    color_setting!(text_muted, "Muted text", false),
    color_setting!(placeholder_text, "Placeholder text", true),
    color_setting!(accent, "Accent", false),
    color_setting!(match_highlight, "Search match", false),
    color_setting!(selected_row, "Selected row", true),
    color_setting!(control_background, "Control background", false),
    color_setting!(control_border, "Control border", false),
    color_setting!(control_hover, "Control hover", false),
    color_setting!(success, "Success", false),
    color_setting!(error, "Error", false),
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
        )
        .description("Follow the system appearance or force a light or dark theme.");

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
        )
        .description("Theme used whenever Glyphst is in light appearance.");

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
        )
        .description("Theme used whenever Glyphst is in dark appearance.");

        let themes_directory = user_themes_dir();
        let themes_directory_label = themes_directory.display().to_string();
        let theme_directory = SettingItem::new(
            "User theme directory",
            SettingField::render(move |options, _, _| {
                let path = themes_directory.clone();
                Button::new("open-user-theme-directory")
                    .label("Show Directory")
                    .outline()
                    .with_size(options.size)
                    .on_click(move |_, _, cx| cx.reveal_path(&path))
            }),
        )
        .description(format!(
            "Place gpui-components ThemeSet JSON files in {themes_directory_label}. \
             Glyphst loads them at startup and merges them into both theme catalogs."
        ));

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
        let font_fallbacks = SettingItem::new("UI font fallbacks", fallback_field)
            .layout(Axis::Vertical)
            .description(crate::fl!("settings-font-fallbacks-hint"));

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
        )
        .description("Spacing between pages in continuous document layouts.");

        let document_colors = DOCUMENT_COLORS
            .into_iter()
            .map(|setting| Self::color_setting_item(setting, view.clone()));
        let chrome_colors = CHROME_COLORS
            .into_iter()
            .map(|setting| Self::color_setting_item(setting, view.clone()));
        let control_colors = CONTROL_COLORS
            .into_iter()
            .map(|setting| Self::color_setting_item(setting, view.clone()));

        SettingPage::new(crate::fl!("settings-tab-appearance"))
            .description(
                "Choose component themes first, then apply optional Glyphst-specific semantic overrides.",
            )
            .groups([
                SettingGroup::new()
                    .title(crate::fl!("settings-section-theme"))
                    .items([mode, light_theme, dark_theme, theme_directory]),
                SettingGroup::new()
                    .title(crate::fl!("settings-section-fonts"))
                    .items([font_family, font_fallbacks, font_size, font_weight]),
                SettingGroup::new()
                    .title("Document layout")
                    .item(page_gap),
                SettingGroup::new()
                    .title("Document colors")
                    .description("Colors used by document pages, selections, annotations, and overlays.")
                    .items(document_colors),
                SettingGroup::new()
                    .title("Window chrome")
                    .description("Glyphst-specific footer, panel, border, and tab semantics.")
                    .items(chrome_colors),
                SettingGroup::new()
                    .title("Text and controls")
                    .description("Glyphst-specific text, state, and control semantics.")
                    .items(control_colors),
            ])
    }
}
