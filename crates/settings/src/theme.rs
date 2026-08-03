use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{Context as _, Result};
use glyphst_core::HighlightColor;
use gpui::{App, Hsla, rgb, rgba};
use gpui_component::{
    Colorize as _, Theme as ComponentTheme, ThemeConfig as ComponentThemeConfig,
    ThemeMode as ComponentThemeMode, ThemeRegistry as ComponentThemeRegistry,
    ThemeSet as ComponentThemeSet,
};
use kdl::{KdlDocument, KdlNode, KdlValue};

pub const DEFAULT_LIGHT_THEME_ID: &str = "Default Light";
pub const DEFAULT_DARK_THEME_ID: &str = "Default Dark";
pub const DEFAULT_PAGE_GAP: f32 = 4.0;
pub const SYNCHRONIZED_GROUP_COLOR_COUNT: usize = 8;

const MIN_PAGE_GAP: f32 = 0.0;
const MAX_PAGE_GAP: f32 = 240.0;

const BUNDLED_THEME_SOURCES: [&str; 23] = [
    include_str!("../themes/gpui-component/default-theme.json"),
    include_str!("../themes/gpui-component/adventure.json"),
    include_str!("../themes/gpui-component/alduin.json"),
    include_str!("../themes/gpui-component/asciinema.json"),
    include_str!("../themes/gpui-component/aurora.json"),
    include_str!("../themes/gpui-component/ayu.json"),
    include_str!("../themes/gpui-component/catppuccin.json"),
    include_str!("../themes/gpui-component/everforest.json"),
    include_str!("../themes/gpui-component/fahrenheit.json"),
    include_str!("../themes/gpui-component/flexoki.json"),
    include_str!("../themes/gpui-component/gruvbox.json"),
    include_str!("../themes/gpui-component/harper.json"),
    include_str!("../themes/gpui-component/hybrid.json"),
    include_str!("../themes/gpui-component/jellybeans.json"),
    include_str!("../themes/gpui-component/kibble.json"),
    include_str!("../themes/gpui-component/macos-classic.json"),
    include_str!("../themes/gpui-component/matrix.json"),
    include_str!("../themes/gpui-component/mellifluous.json"),
    include_str!("../themes/gpui-component/molokai.json"),
    include_str!("../themes/gpui-component/solarized.json"),
    include_str!("../themes/gpui-component/spaceduck.json"),
    include_str!("../themes/gpui-component/tokyonight.json"),
    include_str!("../themes/gpui-component/twilight.json"),
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

    fn from_component(mode: ComponentThemeMode) -> Self {
        if mode.is_dark() {
            Self::Dark
        } else {
            Self::Light
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

/// A light/dark theme pair and the rule used to choose between them.
///
/// Glyphst intentionally has no single-theme compatibility mode. Keeping both
/// choices explicit makes system appearance changes deterministic.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeSelection {
    pub mode: ThemeAppearanceMode,
    pub light: String,
    pub dark: String,
}

impl Default for ThemeSelection {
    fn default() -> Self {
        Self {
            mode: ThemeAppearanceMode::System,
            light: DEFAULT_LIGHT_THEME_ID.to_string(),
            dark: DEFAULT_DARK_THEME_ID.to_string(),
        }
    }
}

impl ThemeSelection {
    /// Returns the theme name for the selected mode.
    pub fn name(&self, system_appearance: ThemeAppearance) -> &str {
        match self.mode {
            ThemeAppearanceMode::Light => &self.light,
            ThemeAppearanceMode::Dark => &self.dark,
            ThemeAppearanceMode::System => match system_appearance {
                ThemeAppearance::Light => &self.light,
                ThemeAppearance::Dark => &self.dark,
            },
        }
    }

    /// Returns the mode for the theme selection.
    pub fn mode(&self) -> ThemeAppearanceMode {
        self.mode
    }

    pub fn effective_appearance(&self, system_appearance: ThemeAppearance) -> ThemeAppearance {
        match self.mode {
            ThemeAppearanceMode::Light => ThemeAppearance::Light,
            ThemeAppearanceMode::Dark => ThemeAppearance::Dark,
            ThemeAppearanceMode::System => system_appearance,
        }
    }

    /// Sets the mode for the theme.
    pub fn set_mode(&mut self, mode: ThemeAppearanceMode) {
        self.mode = mode;
    }

    /// Sets the theme for the given appearance to the theme with the specified name.
    pub fn set_theme(
        &mut self,
        name: String,
        appearance: ThemeAppearance,
        system_appearance: ThemeAppearance,
    ) {
        match appearance {
            ThemeAppearance::Light => self.light = name,
            ThemeAppearance::Dark => self.dark = name,
        }

        let should_update_mode =
            !(self.mode == ThemeAppearanceMode::System && appearance == system_appearance);

        if should_update_mode {
            self.mode = appearance_to_mode(appearance);
        }
    }

    pub fn sanitize(&mut self) {
        if self.light.trim().is_empty() {
            self.light = DEFAULT_LIGHT_THEME_ID.to_string();
        }
        if self.dark.trim().is_empty() {
            self.dark = DEFAULT_DARK_THEME_ID.to_string();
        }
    }

    pub fn from_kdl_node(node: &KdlNode) -> Self {
        let Some(children) = node.children() else {
            return Self::default();
        };

        Self {
            mode: children
                .get_arg("mode")
                .and_then(KdlValue::as_string)
                .and_then(ThemeAppearanceMode::from_str)
                .unwrap_or_default(),
            light: children
                .get_arg("light")
                .and_then(KdlValue::as_string)
                .unwrap_or(DEFAULT_LIGHT_THEME_ID)
                .to_string(),
            dark: children
                .get_arg("dark")
                .and_then(KdlValue::as_string)
                .unwrap_or(DEFAULT_DARK_THEME_ID)
                .to_string(),
        }
    }

    pub fn write_to_document(self, document: &mut KdlDocument) {
        let mut theme_node = KdlNode::new("theme");
        let children = theme_node.ensure_children();
        push_value_node(children, "mode", self.mode.as_str());
        push_value_node(children, "light", self.light);
        push_value_node(children, "dark", self.dark);
        document.nodes_mut().push(theme_node);
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
    pub match_highlight: u32,
    pub synchronized_group_colors: [u32; SYNCHRONIZED_GROUP_COLOR_COUNT],
    pub page_gap: f32,
}

impl Theme {
    /// Resolve Glyphst's document/workspace semantics from the component
    /// theme. This is the only color translation layer retained by Glyphst.
    pub fn from_component(theme: &ComponentTheme) -> Self {
        Self {
            background: hsla_to_rgb(theme.background),
            page: hsla_to_rgb(theme.popover),
            selection_highlight: hsla_to_rgba(theme.selection),
            highlight_yellow: hsla_to_rgba_with_alpha(theme.yellow, 0x66),
            highlight_green: hsla_to_rgba_with_alpha(theme.green, 0x66),
            highlight_blue: hsla_to_rgba_with_alpha(theme.blue, 0x66),
            highlight_pink: hsla_to_rgba_with_alpha(theme.magenta, 0x66),
            highlight_orange: hsla_to_rgba_with_alpha(theme.warning, 0x66),
            match_highlight: hsla_to_rgb(theme.warning),
            synchronized_group_colors: [
                hsla_to_rgb(theme.red),
                hsla_to_rgb(theme.red.mix_oklab(theme.yellow, 0.5)),
                hsla_to_rgb(theme.yellow),
                hsla_to_rgb(theme.green),
                hsla_to_rgb(theme.cyan),
                hsla_to_rgb(theme.blue),
                hsla_to_rgb(theme.blue.mix_oklab(theme.magenta, 0.5)),
                hsla_to_rgb(theme.magenta),
            ],
            page_gap: DEFAULT_PAGE_GAP,
        }
    }

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
        apply!(match_highlight);

        for (color, override_color) in self
            .synchronized_group_colors
            .iter_mut()
            .zip(partial.synchronized_group_colors)
        {
            if let Some(override_color) = override_color {
                *color = override_color;
            }
        }

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
        mask_rgb(&mut self.match_highlight);
        for color in &mut self.synchronized_group_colors {
            mask_rgb(color);
        }
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

    pub fn synchronized_group_color(self, color_slot: usize) -> u32 {
        self.synchronized_group_colors[color_slot % SYNCHRONIZED_GROUP_COLOR_COUNT]
    }
}

impl Default for Theme {
    fn default() -> Self {
        let set = serde_json::from_str::<ComponentThemeSet>(BUNDLED_THEME_SOURCES[0])
            .expect("bundled gpui-component default theme is invalid");
        let config = set
            .themes
            .into_iter()
            .find(|config| config.name == DEFAULT_LIGHT_THEME_ID)
            .expect("bundled gpui-component default light theme is missing");
        let mut theme = ComponentTheme::default();
        theme.apply_config(&Rc::new(config));
        Self::from_component(&theme)
    }
}

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
    pub synchronized_group_colors: [Option<u32>; SYNCHRONIZED_GROUP_COLOR_COUNT],
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
        for color in &mut self.synchronized_group_colors {
            mask_rgb_option(color);
        }

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
        for (index, color) in self.synchronized_group_colors.into_iter().enumerate() {
            if let Some(color) = color {
                push_value_node(
                    color_children,
                    &format!("synchronized-group-{}", index + 1),
                    format_rgb_hex(color),
                );
            }
        }

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
        for (index, color) in self.synchronized_group_colors.iter_mut().enumerate() {
            *color = colors
                .get_arg(&format!("synchronized-group-{}", index + 1))
                .and_then(KdlValue::as_string)
                .and_then(|value| parse_hex_color(value, false));
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThemeOrigin {
    Bundled,
    User(PathBuf),
}

#[derive(Clone, Debug)]
pub struct ThemeMetadata {
    pub id: String,
    pub name: String,
    pub appearance: ThemeAppearance,
    pub origin: ThemeOrigin,
}

#[derive(Clone, Debug)]
pub struct ThemeCatalog {
    themes: Vec<ThemeEntry>,
}

#[derive(Clone, Debug)]
struct ThemeEntry {
    id: String,
    config: Rc<ComponentThemeConfig>,
    origin: ThemeOrigin,
}

impl ThemeCatalog {
    pub fn load(user_themes_dir: &Path) -> Self {
        let mut catalog = Self::bundled();
        catalog.load_user_themes(user_themes_dir);
        catalog
    }

    pub fn resolve(&self, theme_id: &str, overrides: PartialTheme) -> Theme {
        let Some(mut theme) = self.component_theme(theme_id) else {
            return Theme::default().apply(overrides);
        };
        apply_component_overrides(&mut theme, overrides);
        Theme::from_component(&theme).apply(overrides)
    }

    pub fn contains(&self, theme_id: &str) -> bool {
        self.config(theme_id).is_some()
    }

    pub fn contains_for_appearance(&self, theme_id: &str, appearance: ThemeAppearance) -> bool {
        self.config(theme_id)
            .is_some_and(|config| ThemeAppearance::from_component(config.mode) == appearance)
    }

    pub fn appearance(&self, theme_id: &str) -> ThemeAppearance {
        self.config(theme_id)
            .map(|config| ThemeAppearance::from_component(config.mode))
            .unwrap_or(ThemeAppearance::Light)
    }

    pub fn metadata(&self) -> Vec<ThemeMetadata> {
        let mut themes = self
            .themes
            .iter()
            .map(|theme| ThemeMetadata {
                id: theme.id.clone(),
                name: theme.config.name.to_string(),
                appearance: ThemeAppearance::from_component(theme.config.mode),
                origin: theme.origin.clone(),
            })
            .collect::<Vec<_>>();
        themes.sort_by(|a, b| {
            let a_default = a.id == DEFAULT_LIGHT_THEME_ID || a.id == DEFAULT_DARK_THEME_ID;
            let b_default = b.id == DEFAULT_LIGHT_THEME_ID || b.id == DEFAULT_DARK_THEME_ID;
            b_default
                .cmp(&a_default)
                .then_with(|| {
                    appearance_sort_key(a.appearance).cmp(&appearance_sort_key(b.appearance))
                })
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        themes
    }

    pub fn config(&self, theme_id: &str) -> Option<Rc<ComponentThemeConfig>> {
        self.themes
            .iter()
            .find(|entry| entry.id == theme_id)
            .map(|entry| Rc::clone(&entry.config))
    }

    pub fn install_into_component_registry(&self, cx: &mut App) -> Result<()> {
        let set = ComponentThemeSet {
            name: "Glyphst theme catalog".into(),
            themes: self
                .themes
                .iter()
                .map(|entry| (*entry.config).clone())
                .collect(),
            ..ComponentThemeSet::default()
        };
        let source = serde_json::to_string(&set)?;
        ComponentThemeRegistry::global_mut(cx).load_themes_from_str(&source)
    }

    pub(crate) fn bundled() -> Self {
        let mut catalog = Self { themes: Vec::new() };

        for source in BUNDLED_THEME_SOURCES {
            if let Err(error) = catalog.load_source(source, ThemeOrigin::Bundled) {
                eprintln!("failed to parse bundled gpui-component theme: {error:#}");
            }
        }

        if !catalog.contains(DEFAULT_LIGHT_THEME_ID) || !catalog.contains(DEFAULT_DARK_THEME_ID) {
            panic!("bundled gpui-component default themes are missing");
        }

        catalog
    }

    fn load_user_themes(&mut self, directory: &Path) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };

        let mut paths = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect::<Vec<PathBuf>>();
        paths.sort();

        for path in paths {
            match fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))
                .and_then(|source| self.load_source(&source, ThemeOrigin::User(path.clone())))
            {
                Ok(()) => {}
                Err(error) => eprintln!("failed to load theme {}: {error:#}", path.display()),
            }
        }
    }

    fn load_source(&mut self, source: &str, origin: ThemeOrigin) -> Result<()> {
        let set = serde_json::from_str::<ComponentThemeSet>(source)?;
        let mut known = self
            .themes
            .iter()
            .map(|entry| entry.id.clone())
            .collect::<HashSet<_>>();

        for config in set.themes {
            let id = config.name.trim().to_string();
            if id.is_empty() || !known.insert(id.clone()) {
                continue;
            }
            self.themes.push(ThemeEntry {
                id,
                config: Rc::new(config),
                origin: origin.clone(),
            });
        }

        Ok(())
    }

    fn component_theme(&self, id: &str) -> Option<ComponentTheme> {
        let config = self
            .config(id)
            .or_else(|| self.config(DEFAULT_LIGHT_THEME_ID))?;
        let mut theme = ComponentTheme::default();
        theme.apply_config(&config);
        Some(theme)
    }
}

fn appearance_sort_key(appearance: ThemeAppearance) -> u8 {
    match appearance {
        ThemeAppearance::Light => 0,
        ThemeAppearance::Dark => 1,
    }
}

/// Apply the subset of Glyphst's domain overrides that has a component-theme
/// equivalent. Document-only colors are applied later by [`Theme::apply`].
pub fn apply_component_overrides(theme: &mut ComponentTheme, overrides: PartialTheme) {
    macro_rules! apply_rgb {
        ($source:ident => $($target:ident),+ $(,)?) => {
            if let Some(value) = overrides.$source {
                let color: Hsla = rgb(value).into();
                $(
                    theme.colors.$target = color;
                    theme.tokens.$target = color.into();
                )+
            }
        };
    }

    macro_rules! apply_rgba {
        ($source:ident => $($target:ident),+ $(,)?) => {
            if let Some(value) = overrides.$source {
                let color: Hsla = rgba(value).into();
                $(
                    theme.colors.$target = color;
                    theme.tokens.$target = color.into();
                )+
            }
        };
    }

    apply_rgb!(background => background);
    apply_rgba!(overlay => overlay);
    apply_rgb!(footer_background => status_bar);
    apply_rgb!(footer_border => status_bar_border);
    apply_rgb!(panel_background => sidebar);
    apply_rgb!(panel_border => sidebar_border);
    apply_rgb!(surface_background => popover);
    apply_rgb!(border => border);
    apply_rgb!(border_variant => input);
    apply_rgb!(border_focused => ring);
    apply_rgb!(tab_bar_background => tab_bar);
    apply_rgb!(tab_active_background => tab_active);
    apply_rgb!(tab_inactive_background => tab);
    apply_rgb!(element_hover => list_hover);
    apply_rgb!(element_selected => list_active);
    apply_rgb!(text => foreground, popover_foreground, sidebar_foreground);
    apply_rgb!(text_muted => muted_foreground);
    apply_rgba!(placeholder_text => muted_foreground);
    apply_rgb!(accent => primary, ring, link, caret, sidebar_primary);
    apply_rgb!(match_highlight => warning, yellow);
    apply_rgba!(selected_row => list_active, table_active);
    apply_rgb!(control_background => button);
    apply_rgb!(control_border => input);
    apply_rgb!(control_hover => button_hover);
    apply_rgb!(success => success);
    apply_rgb!(error => danger);
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

fn hsla_to_rgb(color: Hsla) -> u32 {
    hsla_to_rgba(color) >> 8
}

fn hsla_to_rgba(color: Hsla) -> u32 {
    let rgb = color.to_rgb();
    (channel_to_u8(rgb.r) << 24)
        | (channel_to_u8(rgb.g) << 16)
        | (channel_to_u8(rgb.b) << 8)
        | channel_to_u8(color.a)
}

fn hsla_to_rgba_with_alpha(color: Hsla, alpha: u32) -> u32 {
    (hsla_to_rgb(color) << 8) | (alpha & 0xff)
}

fn channel_to_u8(channel: f32) -> u32 {
    (channel.clamp(0.0, 1.0) * 255.0).round() as u32
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
