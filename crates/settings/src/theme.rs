use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use kdl::{KdlDocument, KdlNode, KdlValue};

use glyphst_library::HighlightColor;

pub const DEFAULT_THEME_ID: &str = "one-light";
pub const DEFAULT_LIGHT_THEME_ID: &str = "one-light";
pub const DEFAULT_DARK_THEME_ID: &str = "one-dark";
pub const DEFAULT_PAGE_GAP: f32 = 4.0;

const MIN_PAGE_GAP: f32 = 0.0;
const MAX_PAGE_GAP: f32 = 240.0;

const BUILTIN_THEMES: [(&str, &str); 8] = [
    ("one-light", include_str!("../themes/one-light.kdl")),
    ("one-dark", include_str!("../themes/one-dark.kdl")),
    (
        "modus-operandi",
        include_str!("../themes/modus-operandi.kdl"),
    ),
    ("modus-vivendi", include_str!("../themes/modus-vivendi.kdl")),
    (
        "modus-operandi-tinted",
        include_str!("../themes/modus-operandi-tinted.kdl"),
    ),
    (
        "modus-operandi-deuteranopia",
        include_str!("../themes/modus-operandi-deuteranopia.kdl"),
    ),
    (
        "modus-vivendi-tinted",
        include_str!("../themes/modus-vivendi-tinted.kdl"),
    ),
    (
        "modus-vivendi-deuteranopia",
        include_str!("../themes/modus-vivendi-deuteranopia.kdl"),
    ),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeAppearance {
    Dark,
    Light,
}

impl ThemeAppearance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }
}

/// The mode used to select a theme.
///
/// `Light` and `Dark` will select their respective themes.
///
/// `System` will select the theme based on the system's appearance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeAppearanceMode {
    Light,
    Dark,
    #[default]
    System,
}

impl ThemeAppearanceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

pub fn appearance_to_mode(appearance: ThemeAppearance) -> ThemeAppearanceMode {
    match appearance {
        ThemeAppearance::Light => ThemeAppearanceMode::Light,
        ThemeAppearance::Dark => ThemeAppearanceMode::Dark,
    }
}

/// Represents the selection of a theme, which can be either static or dynamic.
#[derive(Clone, Debug, PartialEq)]
pub enum ThemeSelection {
    /// A static theme selection, represented by a single theme name.
    Static(String),
    /// A dynamic theme selection, which can change based on the mode.
    Dynamic {
        mode: ThemeAppearanceMode,
        light: String,
        dark: String,
    },
}

impl Default for ThemeSelection {
    fn default() -> Self {
        Self::Static(DEFAULT_THEME_ID.to_string())
    }
}

impl ThemeSelection {
    /// Returns the theme name for the selected mode.
    pub fn name(&self, system_appearance: ThemeAppearance) -> &str {
        match self {
            Self::Static(theme) => theme,
            Self::Dynamic { mode, light, dark } => match mode {
                ThemeAppearanceMode::Light => light,
                ThemeAppearanceMode::Dark => dark,
                ThemeAppearanceMode::System => match system_appearance {
                    ThemeAppearance::Light => light,
                    ThemeAppearance::Dark => dark,
                },
            },
        }
    }

    /// Returns the mode for the theme selection.
    pub fn mode(&self) -> Option<ThemeAppearanceMode> {
        match self {
            Self::Static(_) => None,
            Self::Dynamic { mode, .. } => Some(*mode),
        }
    }

    /// Sets the mode for the theme.
    ///
    /// If the current selection is `Static`, this migrates it to `Dynamic`
    /// with `mode: System` and the default light/dark themes.
    /// If the current selection is `Dynamic`, this updates the mode field.
    pub fn set_mode(&mut self, mode: ThemeAppearanceMode) {
        match self {
            Self::Static(_) => {
                *self = Self::Dynamic {
                    mode: ThemeAppearanceMode::System,
                    light: DEFAULT_LIGHT_THEME_ID.to_string(),
                    dark: DEFAULT_DARK_THEME_ID.to_string(),
                };
            }
            Self::Dynamic {
                mode: mode_to_update,
                ..
            } => *mode_to_update = mode,
        }
    }

    /// Sets the theme for the given appearance to the theme with the specified name.
    pub fn set_theme(
        &mut self,
        name: String,
        appearance: ThemeAppearance,
        system_appearance: ThemeAppearance,
    ) {
        match self {
            Self::Static(current) => *current = name,
            Self::Dynamic { mode, light, dark } => {
                match appearance {
                    ThemeAppearance::Light => *light = name,
                    ThemeAppearance::Dark => *dark = name,
                }

                let should_update_mode =
                    !(*mode == ThemeAppearanceMode::System && appearance == system_appearance);

                if should_update_mode {
                    *mode = appearance_to_mode(appearance);
                }
            }
        }
    }

    pub fn sanitize(&mut self) {
        match self {
            Self::Static(name) => {
                if name.trim().is_empty() {
                    *name = DEFAULT_THEME_ID.to_string();
                }
            }
            Self::Dynamic { light, dark, .. } => {
                if light.trim().is_empty() {
                    *light = DEFAULT_LIGHT_THEME_ID.to_string();
                }
                if dark.trim().is_empty() {
                    *dark = DEFAULT_DARK_THEME_ID.to_string();
                }
            }
        }
    }

    pub fn from_kdl_node(node: &KdlNode) -> Self {
        if let Some(children) = node.children() {
            let mode = children
                .get_arg("mode")
                .and_then(KdlValue::as_string)
                .and_then(ThemeAppearanceMode::from_str)
                .unwrap_or_default();
            let light = children
                .get_arg("light")
                .and_then(KdlValue::as_string)
                .unwrap_or(DEFAULT_LIGHT_THEME_ID)
                .to_string();
            let dark = children
                .get_arg("dark")
                .and_then(KdlValue::as_string)
                .unwrap_or(DEFAULT_DARK_THEME_ID)
                .to_string();
            Self::Dynamic { mode, light, dark }
        } else {
            let name = node
                .get(0)
                .and_then(KdlValue::as_string)
                .unwrap_or(DEFAULT_THEME_ID)
                .to_string();
            Self::Static(name)
        }
    }

    pub fn write_to_document(self, document: &mut KdlDocument) {
        match self {
            Self::Static(name) => {
                push_value_node(document, "theme", name);
            }
            Self::Dynamic { mode, light, dark } => {
                let mut theme_node = KdlNode::new("theme");
                let children = theme_node.ensure_children();
                push_value_node(children, "mode", mode.as_str());
                push_value_node(children, "light", light);
                push_value_node(children, "dark", dark);
                document.nodes_mut().push(theme_node);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub background: u32,
    pub page: u32,
    pub selection_highlight: u32,
    pub highlight_yellow: u32,
    pub highlight_green: u32,
    pub highlight_blue: u32,
    pub highlight_pink: u32,
    pub highlight_orange: u32,
    pub overlay: u32,
    pub footer_background: u32,
    pub footer_border: u32,
    pub footer_text: u32,
    pub panel_background: u32,
    pub panel_border: u32,
    pub surface_background: u32,
    pub border: u32,
    pub border_variant: u32,
    pub border_focused: u32,
    pub tab_bar_background: u32,
    pub tab_active_background: u32,
    pub tab_inactive_background: u32,
    pub element_hover: u32,
    pub element_selected: u32,
    pub text: u32,
    pub text_muted: u32,
    pub placeholder_text: u32,
    pub accent: u32,
    pub match_highlight: u32,
    pub selected_row: u32,
    pub control_background: u32,
    pub control_border: u32,
    pub control_hover: u32,
    pub success: u32,
    pub error: u32,
    pub page_gap: f32,
}

impl Theme {
    pub fn apply(mut self, partial: PartialTheme) -> Self {
        macro_rules! apply {
            ($field:ident) => {
                if let Some(value) = partial.$field {
                    self.$field = value;
                }
            };
        }

        apply!(background);
        apply!(page);
        apply!(selection_highlight);
        apply!(highlight_yellow);
        apply!(highlight_green);
        apply!(highlight_blue);
        apply!(highlight_pink);
        apply!(highlight_orange);
        apply!(overlay);
        apply!(footer_background);
        apply!(footer_border);
        apply!(footer_text);
        apply!(panel_background);
        apply!(panel_border);
        apply!(surface_background);
        apply!(border);
        apply!(border_variant);
        apply!(border_focused);
        apply!(tab_bar_background);
        apply!(tab_active_background);
        apply!(tab_inactive_background);
        apply!(element_hover);
        apply!(element_selected);
        apply!(text);
        apply!(text_muted);
        apply!(placeholder_text);
        apply!(accent);
        apply!(match_highlight);
        apply!(selected_row);
        apply!(control_background);
        apply!(control_border);
        apply!(control_hover);
        apply!(success);
        apply!(error);

        self.apply_legacy_token_fallbacks(partial);

        if let Some(page_gap) = partial.page_gap {
            self.page_gap = page_gap;
        }

        self.sanitize();
        self
    }

    pub fn sanitize(&mut self) {
        mask_rgb(&mut self.background);
        mask_rgb(&mut self.page);
        mask_rgba(&mut self.selection_highlight);
        mask_rgba(&mut self.highlight_yellow);
        mask_rgba(&mut self.highlight_green);
        mask_rgba(&mut self.highlight_blue);
        mask_rgba(&mut self.highlight_pink);
        mask_rgba(&mut self.highlight_orange);
        mask_rgba(&mut self.overlay);
        mask_rgb(&mut self.footer_background);
        mask_rgb(&mut self.footer_border);
        mask_rgb(&mut self.footer_text);
        mask_rgb(&mut self.panel_background);
        mask_rgb(&mut self.panel_border);
        mask_rgb(&mut self.surface_background);
        mask_rgb(&mut self.border);
        mask_rgb(&mut self.border_variant);
        mask_rgb(&mut self.border_focused);
        mask_rgb(&mut self.tab_bar_background);
        mask_rgb(&mut self.tab_active_background);
        mask_rgb(&mut self.tab_inactive_background);
        mask_rgb(&mut self.element_hover);
        mask_rgb(&mut self.element_selected);
        mask_rgb(&mut self.text);
        mask_rgb(&mut self.text_muted);
        mask_rgba(&mut self.placeholder_text);
        mask_rgb(&mut self.accent);
        mask_rgb(&mut self.match_highlight);
        mask_rgba(&mut self.selected_row);
        mask_rgb(&mut self.control_background);
        mask_rgb(&mut self.control_border);
        mask_rgb(&mut self.control_hover);
        mask_rgb(&mut self.success);
        mask_rgb(&mut self.error);
        self.page_gap = self.page_gap.clamp(MIN_PAGE_GAP, MAX_PAGE_GAP);
    }

    pub fn highlight_color(self, color: HighlightColor) -> u32 {
        match color {
            HighlightColor::Yellow => self.highlight_yellow,
            HighlightColor::Green => self.highlight_green,
            HighlightColor::Blue => self.highlight_blue,
            HighlightColor::Pink => self.highlight_pink,
            HighlightColor::Orange => self.highlight_orange,
        }
    }

    pub fn note_color(self, color: HighlightColor) -> u32 {
        let base = self.highlight_color(color);
        let scale = |shift: u32| (((base >> shift) & 0xffu32) as f32 * 0.65) as u32;
        (scale(24) << 24) | (scale(16) << 16) | (scale(8) << 8) | 0xff
    }

    fn apply_legacy_token_fallbacks(&mut self, partial: PartialTheme) {
        if partial.surface_background.is_none()
            && let Some(value) = partial.panel_background
        {
            self.surface_background = value;
        }
        if partial.border.is_none()
            && let Some(value) = partial.panel_border
        {
            self.border = value;
        }
        if partial.border_variant.is_none()
            && let Some(value) = partial.control_border.or(partial.panel_border)
        {
            self.border_variant = value;
        }
        if partial.border_focused.is_none()
            && let Some(value) = partial.accent
        {
            self.border_focused = value;
        }
        if partial.tab_bar_background.is_none()
            && let Some(value) = partial.panel_background
        {
            self.tab_bar_background = value;
        }
        if partial.tab_active_background.is_none()
            && let Some(value) = partial.background
        {
            self.tab_active_background = value;
        }
        if partial.tab_inactive_background.is_none()
            && let Some(value) = partial.panel_background
        {
            self.tab_inactive_background = value;
        }
        if partial.element_hover.is_none()
            && let Some(value) = partial.control_hover
        {
            self.element_hover = value;
        }
        if partial.element_selected.is_none()
            && let Some(value) = partial.selected_row
        {
            self.element_selected = rgba_to_rgb(value);
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        DEFAULT_THEME
    }
}

pub const DEFAULT_THEME: Theme = Theme {
    background: 0xdcdcdd,
    page: 0xffffff,
    selection_highlight: 0x5c78e23d,
    highlight_yellow: 0xdec18466,
    highlight_green: 0x669f5966,
    highlight_blue: 0x5c78e266,
    highlight_pink: 0xa349ab66,
    highlight_orange: 0xad6e2666,
    overlay: 0x00000033,
    footer_background: 0xebebec,
    footer_border: 0xc9c9ca,
    footer_text: 0x58585a,
    panel_background: 0xebebec,
    panel_border: 0xc9c9ca,
    surface_background: 0xebebec,
    border: 0xc9c9ca,
    border_variant: 0xdfdfe0,
    border_focused: 0x7d82e8,
    tab_bar_background: 0xebebec,
    tab_active_background: 0xfafafa,
    tab_inactive_background: 0xebebec,
    element_hover: 0xdfdfe0,
    element_selected: 0xcacaca,
    text: 0x242529,
    text_muted: 0x58585a,
    placeholder_text: 0x7e8086ff,
    accent: 0x5c78e2,
    match_highlight: 0xd0a923,
    selected_row: 0xcacacaff,
    control_background: 0xebebec,
    control_border: 0xdfdfe0,
    control_hover: 0xdfdfe0,
    success: 0x669f59,
    error: 0xd36151,
    page_gap: DEFAULT_PAGE_GAP,
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PartialTheme {
    pub background: Option<u32>,
    pub page: Option<u32>,
    pub selection_highlight: Option<u32>,
    pub highlight_yellow: Option<u32>,
    pub highlight_green: Option<u32>,
    pub highlight_blue: Option<u32>,
    pub highlight_pink: Option<u32>,
    pub highlight_orange: Option<u32>,
    pub overlay: Option<u32>,
    pub footer_background: Option<u32>,
    pub footer_border: Option<u32>,
    pub footer_text: Option<u32>,
    pub panel_background: Option<u32>,
    pub panel_border: Option<u32>,
    pub surface_background: Option<u32>,
    pub border: Option<u32>,
    pub border_variant: Option<u32>,
    pub border_focused: Option<u32>,
    pub tab_bar_background: Option<u32>,
    pub tab_active_background: Option<u32>,
    pub tab_inactive_background: Option<u32>,
    pub element_hover: Option<u32>,
    pub element_selected: Option<u32>,
    pub text: Option<u32>,
    pub text_muted: Option<u32>,
    pub placeholder_text: Option<u32>,
    pub accent: Option<u32>,
    pub match_highlight: Option<u32>,
    pub selected_row: Option<u32>,
    pub control_background: Option<u32>,
    pub control_border: Option<u32>,
    pub control_hover: Option<u32>,
    pub success: Option<u32>,
    pub error: Option<u32>,
    pub page_gap: Option<f32>,
}

impl PartialTheme {
    pub fn from_document(document: &KdlDocument) -> Self {
        let mut partial = Self::default();

        if let Some(colors) = document.get("colors").and_then(KdlNode::children) {
            partial.read_colors(colors);
        }

        if let Some(value) = document.get_arg("page-gap").and_then(value_to_f32) {
            partial.page_gap = Some(value);
        }

        partial.sanitize();
        partial
    }

    pub fn sanitize(&mut self) {
        mask_rgb_option(&mut self.background);
        mask_rgb_option(&mut self.page);
        mask_rgba_option(&mut self.selection_highlight);
        mask_rgba_option(&mut self.highlight_yellow);
        mask_rgba_option(&mut self.highlight_green);
        mask_rgba_option(&mut self.highlight_blue);
        mask_rgba_option(&mut self.highlight_pink);
        mask_rgba_option(&mut self.highlight_orange);
        mask_rgba_option(&mut self.overlay);
        mask_rgb_option(&mut self.footer_background);
        mask_rgb_option(&mut self.footer_border);
        mask_rgb_option(&mut self.footer_text);
        mask_rgb_option(&mut self.panel_background);
        mask_rgb_option(&mut self.panel_border);
        mask_rgb_option(&mut self.surface_background);
        mask_rgb_option(&mut self.border);
        mask_rgb_option(&mut self.border_variant);
        mask_rgb_option(&mut self.border_focused);
        mask_rgb_option(&mut self.tab_bar_background);
        mask_rgb_option(&mut self.tab_active_background);
        mask_rgb_option(&mut self.tab_inactive_background);
        mask_rgb_option(&mut self.element_hover);
        mask_rgb_option(&mut self.element_selected);
        mask_rgb_option(&mut self.text);
        mask_rgb_option(&mut self.text_muted);
        mask_rgba_option(&mut self.placeholder_text);
        mask_rgb_option(&mut self.accent);
        mask_rgb_option(&mut self.match_highlight);
        mask_rgba_option(&mut self.selected_row);
        mask_rgb_option(&mut self.control_background);
        mask_rgb_option(&mut self.control_border);
        mask_rgb_option(&mut self.control_hover);
        mask_rgb_option(&mut self.success);
        mask_rgb_option(&mut self.error);

        if let Some(page_gap) = self.page_gap.as_mut() {
            *page_gap = page_gap.clamp(MIN_PAGE_GAP, MAX_PAGE_GAP);
        }
    }

    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    pub fn write_to_document(self, document: &mut KdlDocument) {
        let mut colors = KdlNode::new("colors");
        let color_children = colors.ensure_children();

        macro_rules! write_color {
            ($field:ident, $name:literal, $formatter:ident) => {
                if let Some(value) = self.$field {
                    push_value_node(color_children, $name, $formatter(value));
                }
            };
        }

        write_color!(background, "background", format_rgb_hex);
        write_color!(page, "page", format_rgb_hex);
        write_color!(selection_highlight, "selection-highlight", format_rgba_hex);
        write_color!(highlight_yellow, "highlight-yellow", format_rgba_hex);
        write_color!(highlight_green, "highlight-green", format_rgba_hex);
        write_color!(highlight_blue, "highlight-blue", format_rgba_hex);
        write_color!(highlight_pink, "highlight-pink", format_rgba_hex);
        write_color!(highlight_orange, "highlight-orange", format_rgba_hex);
        write_color!(overlay, "overlay", format_rgba_hex);
        write_color!(footer_background, "footer-background", format_rgb_hex);
        write_color!(footer_border, "footer-border", format_rgb_hex);
        write_color!(footer_text, "footer-text", format_rgb_hex);
        write_color!(panel_background, "panel-background", format_rgb_hex);
        write_color!(panel_border, "panel-border", format_rgb_hex);
        write_color!(surface_background, "surface-background", format_rgb_hex);
        write_color!(border, "border", format_rgb_hex);
        write_color!(border_variant, "border-variant", format_rgb_hex);
        write_color!(border_focused, "border-focused", format_rgb_hex);
        write_color!(tab_bar_background, "tab-bar-background", format_rgb_hex);
        write_color!(
            tab_active_background,
            "tab-active-background",
            format_rgb_hex
        );
        write_color!(
            tab_inactive_background,
            "tab-inactive-background",
            format_rgb_hex
        );
        write_color!(element_hover, "element-hover", format_rgb_hex);
        write_color!(element_selected, "element-selected", format_rgb_hex);
        write_color!(text, "text", format_rgb_hex);
        write_color!(text_muted, "text-muted", format_rgb_hex);
        write_color!(placeholder_text, "placeholder-text", format_rgba_hex);
        write_color!(accent, "accent", format_rgb_hex);
        write_color!(match_highlight, "match-highlight", format_rgb_hex);
        write_color!(selected_row, "selected-row", format_rgba_hex);
        write_color!(control_background, "control-background", format_rgb_hex);
        write_color!(control_border, "control-border", format_rgb_hex);
        write_color!(control_hover, "control-hover", format_rgb_hex);
        write_color!(success, "success", format_rgb_hex);
        write_color!(error, "error", format_rgb_hex);

        if !color_children.nodes().is_empty() {
            document.nodes_mut().push(colors);
        }

        if let Some(page_gap) = self.page_gap {
            push_value_node(document, "page-gap", page_gap as f64);
        }
    }

    fn read_colors(&mut self, colors: &KdlDocument) {
        macro_rules! read_color {
            ($field:ident, $name:literal, $alpha:literal) => {
                if let Some(value) = colors
                    .get_arg($name)
                    .and_then(KdlValue::as_string)
                    .and_then(|value| parse_hex_color(value, $alpha))
                {
                    self.$field = Some(value);
                }
            };
        }

        read_color!(background, "background", false);
        read_color!(page, "page", false);
        read_color!(selection_highlight, "selection-highlight", true);
        read_color!(highlight_yellow, "highlight-yellow", true);
        read_color!(highlight_green, "highlight-green", true);
        read_color!(highlight_blue, "highlight-blue", true);
        read_color!(highlight_pink, "highlight-pink", true);
        read_color!(highlight_orange, "highlight-orange", true);
        read_color!(overlay, "overlay", true);
        read_color!(footer_background, "footer-background", false);
        read_color!(footer_border, "footer-border", false);
        read_color!(footer_text, "footer-text", false);
        read_color!(panel_background, "panel-background", false);
        read_color!(panel_border, "panel-border", false);
        read_color!(surface_background, "surface-background", false);
        read_color!(border, "border", false);
        read_color!(border_variant, "border-variant", false);
        read_color!(border_focused, "border-focused", false);
        read_color!(tab_bar_background, "tab-bar-background", false);
        read_color!(tab_active_background, "tab-active-background", false);
        read_color!(tab_inactive_background, "tab-inactive-background", false);
        read_color!(element_hover, "element-hover", false);
        read_color!(element_selected, "element-selected", false);
        read_color!(text, "text", false);
        read_color!(text_muted, "text-muted", false);
        read_color!(placeholder_text, "placeholder-text", true);
        read_color!(accent, "accent", false);
        read_color!(match_highlight, "match-highlight", false);
        read_color!(selected_row, "selected-row", true);
        read_color!(control_background, "control-background", false);
        read_color!(control_border, "control-border", false);
        read_color!(control_hover, "control-hover", false);
        read_color!(success, "success", false);
        read_color!(error, "error", false);
    }
}

#[derive(Clone, Debug)]
pub struct ThemeMetadata {
    pub id: String,
    pub name: String,
    pub appearance: ThemeAppearance,
}

#[derive(Clone, Debug)]
pub struct ThemeRegistry {
    themes: Vec<ThemeEntry>,
}

#[derive(Clone, Debug)]
struct ThemeEntry {
    id: String,
    name: String,
    appearance: ThemeAppearance,
    theme: Theme,
}

impl ThemeRegistry {
    pub fn load(user_themes_dir: &Path) -> Self {
        let mut registry = Self::builtins();
        registry.load_user_themes(user_themes_dir);
        registry
    }

    pub fn resolve(&self, theme_id: &str, overrides: PartialTheme) -> Theme {
        self.theme(theme_id)
            .or_else(|| self.theme(DEFAULT_THEME_ID))
            .unwrap_or(DEFAULT_THEME)
            .apply(overrides)
    }

    pub fn contains(&self, theme_id: &str) -> bool {
        self.theme(theme_id).is_some()
    }

    pub fn appearance(&self, theme_id: &str) -> ThemeAppearance {
        self.themes
            .iter()
            .find(|entry| entry.id == theme_id)
            .map(|entry| entry.appearance)
            .unwrap_or(ThemeAppearance::Dark)
    }

    pub fn metadata(&self) -> Vec<ThemeMetadata> {
        self.themes
            .iter()
            .map(|theme| ThemeMetadata {
                id: theme.id.clone(),
                name: theme.name.clone(),
                appearance: theme.appearance,
            })
            .collect()
    }

    fn builtins() -> Self {
        let mut registry = Self { themes: Vec::new() };

        for (id, source) in BUILTIN_THEMES {
            match parse_theme_source(id, source) {
                Ok(theme) => registry.insert_theme(theme),
                Err(error) => eprintln!("failed to parse built-in theme {id}: {error:#}"),
            }
        }

        if registry.theme(DEFAULT_THEME_ID).is_none() {
            registry.insert_theme(ThemeEntry {
                id: DEFAULT_THEME_ID.to_string(),
                name: "One Light".to_string(),
                appearance: ThemeAppearance::Light,
                theme: DEFAULT_THEME,
            });
        }

        registry
    }

    fn load_user_themes(&mut self, directory: &Path) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };

        let mut paths = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "kdl"))
            .collect::<Vec<PathBuf>>();
        paths.sort();

        for path in paths {
            let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };

            match fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))
                .and_then(|source| parse_theme_source(id, &source))
            {
                Ok(theme) => self.insert_theme(theme),
                Err(error) => eprintln!("failed to load theme {}: {error:#}", path.display()),
            }
        }
    }

    fn insert_theme(&mut self, theme: ThemeEntry) {
        self.themes.retain(|existing| existing.id != theme.id);
        self.themes.push(theme);
    }

    fn theme(&self, id: &str) -> Option<Theme> {
        self.themes
            .iter()
            .find_map(|entry| (entry.id == id).then_some(entry.theme))
    }
}

fn parse_theme_source(id: &str, source: &str) -> Result<ThemeEntry> {
    let document = source.parse::<KdlDocument>()?;
    let partial = PartialTheme::from_document(&document);
    let name = document
        .get_arg("name")
        .and_then(KdlValue::as_string)
        .unwrap_or(id)
        .to_string();
    let appearance = document
        .get_arg("appearance")
        .and_then(KdlValue::as_string)
        .and_then(ThemeAppearance::from_str)
        .unwrap_or(ThemeAppearance::Dark);

    Ok(ThemeEntry {
        id: id.to_string(),
        name,
        appearance,
        theme: DEFAULT_THEME.apply(partial),
    })
}

pub fn parse_hex_color(value: &str, allow_alpha: bool) -> Option<u32> {
    let value = value.strip_prefix('#').unwrap_or(value);
    let valid_len = value.len() == 6 || (allow_alpha && value.len() == 8);
    if !valid_len || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }

    u32::from_str_radix(value, 16).ok()
}

pub fn format_rgb_hex(value: u32) -> String {
    format!("#{:06x}", value & 0x00ff_ffff)
}

pub fn format_rgba_hex(value: u32) -> String {
    format!("#{:08x}", value)
}

pub fn value_to_f32(value: &KdlValue) -> Option<f32> {
    match value {
        KdlValue::Float(value) if value.is_finite() => Some(*value as f32),
        KdlValue::Integer(value) => Some(*value as f32),
        _ => None,
    }
}

pub fn push_value_node(document: &mut KdlDocument, name: &str, value: impl Into<KdlValue>) {
    let mut node = KdlNode::new(name);
    node.push(value);
    document.nodes_mut().push(node);
}

fn rgba_to_rgb(value: u32) -> u32 {
    (value >> 8) & 0x00ff_ffff
}

fn mask_rgb(value: &mut u32) {
    *value &= 0x00ff_ffff;
}

fn mask_rgba(_: &mut u32) {}

fn mask_rgb_option(value: &mut Option<u32>) {
    if let Some(value) = value.as_mut() {
        mask_rgb(value);
    }
}

fn mask_rgba_option(value: &mut Option<u32>) {
    if let Some(value) = value.as_mut() {
        mask_rgba(value);
    }
}
