use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use kdl::{KdlDocument, KdlNode, KdlValue};

use crate::library::HighlightColor;

pub const DEFAULT_THEME_ID: &str = "glyphst-dark";
pub const DEFAULT_PAGE_GAP: f32 = 4.0;

const MIN_PAGE_GAP: f32 = 0.0;
const MAX_PAGE_GAP: f32 = 240.0;

const BUILTIN_THEMES: [(&str, &str); 4] = [
    ("glyphst-dark", include_str!("../themes/glyphst-dark.kdl")),
    ("glyphst-light", include_str!("../themes/glyphst-light.kdl")),
    ("one-dark", include_str!("../themes/one-dark.kdl")),
    (
        "catppuccin-mocha",
        include_str!("../themes/catppuccin-mocha.kdl"),
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
}

impl Default for Theme {
    fn default() -> Self {
        DEFAULT_THEME
    }
}

pub const DEFAULT_THEME: Theme = Theme {
    background: 0x202020,
    page: 0xffffff,
    selection_highlight: 0x61afef66,
    highlight_yellow: 0xe5c07b66,
    highlight_green: 0x98c37966,
    highlight_blue: 0x61afef66,
    highlight_pink: 0xc678dd66,
    highlight_orange: 0xd19a6666,
    overlay: 0x00000066,
    footer_background: 0x181818,
    footer_border: 0x303030,
    footer_text: 0xaaaaaa,
    panel_background: 0x181818,
    panel_border: 0x343434,
    text: 0xd4d4d4,
    text_muted: 0x888888,
    placeholder_text: 0x888888aa,
    accent: 0x61afef,
    match_highlight: 0xe5c07b,
    selected_row: 0x3a3f4bcc,
    control_background: 0x242424,
    control_border: 0x3a3a3a,
    control_hover: 0x303030,
    success: 0x98c379,
    error: 0xe06c75,
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
                name: "Glyphst Dark".to_string(),
                appearance: ThemeAppearance::Dark,
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
