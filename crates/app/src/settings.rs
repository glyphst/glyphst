use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use gpui::{Global, WindowBackgroundAppearance};
use kdl::{KdlDocument, KdlNode, KdlValue};

use crate::theme::{
    DEFAULT_THEME_ID, PartialTheme, Theme, ThemeRegistry, push_value_node, value_to_f32,
};

pub const DEFAULT_ZOOM: f32 = 1.0;
pub const DEFAULT_ZOOM_STEP: f32 = 1.05;
pub const DEFAULT_SMOOTH_SCROLLING: bool = true;

const MIN_ZOOM_SETTING: f32 = 0.25;
const MAX_ZOOM_SETTING: f32 = 8.0;
const MIN_ZOOM_STEP: f32 = 1.01;
const MAX_ZOOM_STEP: f32 = 4.0;
const DEFAULT_WINDOW_OPACITY: f32 = 1.0;
const MIN_WINDOW_OPACITY: f32 = 0.3;
const MAX_WINDOW_OPACITY: f32 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BehaviorSettings {
    pub default_zoom: f32,
    pub zoom_step: f32,
    pub smooth_scrolling: bool,
}

impl Default for BehaviorSettings {
    fn default() -> Self {
        Self {
            default_zoom: DEFAULT_ZOOM,
            zoom_step: DEFAULT_ZOOM_STEP,
            smooth_scrolling: DEFAULT_SMOOTH_SCROLLING,
        }
    }
}

impl BehaviorSettings {
    pub fn sanitize(&mut self) {
        self.default_zoom = self.default_zoom.clamp(MIN_ZOOM_SETTING, MAX_ZOOM_SETTING);
        self.zoom_step = self.zoom_step.clamp(MIN_ZOOM_STEP, MAX_ZOOM_STEP);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowBackgroundMode {
    Opaque,
    Transparent,
    #[default]
    Blurred,
}

impl WindowBackgroundMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Opaque => "opaque",
            Self::Transparent => "transparent",
            Self::Blurred => "blurred",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "opaque" => Some(Self::Opaque),
            "transparent" => Some(Self::Transparent),
            "blurred" | "blur" => Some(Self::Blurred),
            _ => None,
        }
    }

    fn into_gpui(self) -> WindowBackgroundAppearance {
        match self {
            Self::Opaque => WindowBackgroundAppearance::Opaque,
            Self::Transparent => WindowBackgroundAppearance::Transparent,
            Self::Blurred => WindowBackgroundAppearance::Blurred,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowSettings {
    pub opacity: f32,
    pub background: WindowBackgroundMode,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            opacity: DEFAULT_WINDOW_OPACITY,
            background: WindowBackgroundMode::Blurred,
        }
    }
}

impl WindowSettings {
    pub fn sanitize(&mut self) {
        self.opacity = self.opacity.clamp(MIN_WINDOW_OPACITY, MAX_WINDOW_OPACITY);
    }

    pub fn transparency_enabled_by_default(self) -> bool {
        self.opacity < MAX_WINDOW_OPACITY && self.background != WindowBackgroundMode::Opaque
    }

    pub fn effective_opacity(self, enabled: bool) -> f32 {
        if enabled && self.transparency_enabled_by_default() {
            self.opacity
        } else {
            MAX_WINDOW_OPACITY
        }
    }

    pub fn background_appearance(self, enabled: bool) -> WindowBackgroundAppearance {
        if enabled && self.transparency_enabled_by_default() {
            self.background.into_gpui()
        } else {
            WindowBackgroundAppearance::Opaque
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeymapPreset {
    #[default]
    Modern,
    Vim,
}

impl KeymapPreset {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Modern => "modern",
            Self::Vim => "vim",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Modern => "Modern",
            Self::Vim => "Vim",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "modern" => Some(Self::Modern),
            "vim" | "vim-like" | "vim_like" => Some(Self::Vim),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyBindingOverride {
    pub action: String,
    pub keystrokes: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub behavior: BehaviorSettings,
    pub window: WindowSettings,
    pub keymap_preset: KeymapPreset,
    pub keybinding_overrides: Vec<KeyBindingOverride>,
    pub theme_name: String,
    pub overrides: PartialTheme,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            behavior: BehaviorSettings::default(),
            window: WindowSettings::default(),
            keymap_preset: KeymapPreset::default(),
            keybinding_overrides: Vec::new(),
            theme_name: DEFAULT_THEME_ID.to_string(),
            overrides: PartialTheme::default(),
        }
    }
}

impl Settings {
    pub fn sanitize(&mut self) {
        self.behavior.sanitize();
        self.window.sanitize();
        self.overrides.sanitize();
        if self.theme_name.trim().is_empty() {
            self.theme_name = DEFAULT_THEME_ID.to_string();
        }
        self.keybinding_overrides.retain_mut(|binding| {
            binding.action = binding.action.trim().to_string();
            binding.keystrokes = binding.keystrokes.trim().to_string();
            !binding.action.is_empty() && !binding.keystrokes.is_empty()
        });
    }

    fn from_document(document: &KdlDocument) -> Self {
        let mut settings = Self::default();

        if let Some(theme_name) = document.get_arg("theme").and_then(KdlValue::as_string) {
            settings.theme_name = theme_name.to_string();
        }

        if let Some(preset) = document
            .get_arg("keymap")
            .and_then(KdlValue::as_string)
            .and_then(KeymapPreset::from_str)
        {
            settings.keymap_preset = preset;
        }

        if let Some(zoom) = document.get("zoom").and_then(KdlNode::children) {
            if let Some(value) = zoom.get_arg("default").and_then(value_to_f32) {
                settings.behavior.default_zoom = value;
            }
            if let Some(value) = zoom.get_arg("step").and_then(value_to_f32) {
                settings.behavior.zoom_step = value;
            }
        }

        if let Some(scroll) = document.get("scroll").and_then(KdlNode::children)
            && let Some(value) = scroll.get_arg("smooth").and_then(KdlValue::as_bool)
        {
            settings.behavior.smooth_scrolling = value;
        }

        if let Some(window) = document.get("window").and_then(KdlNode::children) {
            if let Some(value) = window.get_arg("opacity").and_then(value_to_f32) {
                settings.window.opacity = value;
            }
            if let Some(value) = window
                .get_arg("appearance")
                .and_then(KdlValue::as_string)
                .and_then(WindowBackgroundMode::from_str)
            {
                settings.window.background = value;
            }
        }

        if let Some(overrides) = document.get("theme-overrides").and_then(KdlNode::children) {
            settings.overrides = PartialTheme::from_document(overrides);
        }

        if let Some(keybindings) = document.get("keybindings").and_then(KdlNode::children) {
            settings.keybinding_overrides = keybindings
                .nodes()
                .iter()
                .filter_map(|node| {
                    let keystrokes = node.get(0).and_then(KdlValue::as_string)?;
                    Some(KeyBindingOverride {
                        action: node.name().value().to_string(),
                        keystrokes: keystrokes.to_string(),
                    })
                })
                .collect();
        }

        settings.sanitize();
        settings
    }

    pub fn to_kdl_string(&self) -> String {
        let mut document = KdlDocument::new();

        let mut zoom = KdlNode::new("zoom");
        let zoom_children = zoom.ensure_children();
        push_value_node(zoom_children, "default", self.behavior.default_zoom as f64);
        push_value_node(zoom_children, "step", self.behavior.zoom_step as f64);
        document.nodes_mut().push(zoom);

        let mut scroll = KdlNode::new("scroll");
        let scroll_children = scroll.ensure_children();
        push_value_node(scroll_children, "smooth", self.behavior.smooth_scrolling);
        document.nodes_mut().push(scroll);

        push_value_node(&mut document, "theme", self.theme_name.clone());
        push_value_node(&mut document, "keymap", self.keymap_preset.as_str());

        let mut window = KdlNode::new("window");
        let window_children = window.ensure_children();
        push_value_node(window_children, "opacity", self.window.opacity as f64);
        push_value_node(
            window_children,
            "appearance",
            self.window.background.as_str(),
        );
        document.nodes_mut().push(window);

        if !self.keybinding_overrides.is_empty() {
            let mut keybindings = KdlNode::new("keybindings");
            let keybinding_children = keybindings.ensure_children();
            for binding in &self.keybinding_overrides {
                push_value_node(
                    keybinding_children,
                    &binding.action,
                    binding.keystrokes.clone(),
                );
            }
            document.nodes_mut().push(keybindings);
        }

        if !self.overrides.is_empty() {
            let mut overrides = KdlNode::new("theme-overrides");
            self.overrides
                .write_to_document(overrides.ensure_children());
            document.nodes_mut().push(overrides);
        }

        document.autoformat();
        document.to_string()
    }
}

pub struct SettingsStore {
    pub settings: Settings,
    pub registry: ThemeRegistry,
    pub resolved_theme: Theme,
    pub user_managed: bool,
    pub config_path: PathBuf,
    pub state_path: PathBuf,
}

impl Global for SettingsStore {}

impl SettingsStore {
    pub fn load() -> Self {
        let config_path = config_path();
        let state_path = state_path();
        let themes_path = themes_path();
        let registry = ThemeRegistry::load(&themes_path);
        let user_managed = config_path.exists();
        let load_path = if user_managed {
            Some(&config_path)
        } else if state_path.exists() {
            Some(&state_path)
        } else {
            None
        };
        let settings = load_path
            .map(|path| match load_settings(path) {
                Ok(settings) => settings,
                Err(error) => {
                    eprintln!("failed to load settings from {}: {error:#}", path.display());
                    Settings::default()
                }
            })
            .unwrap_or_default();
        let resolved_theme = registry.resolve(&settings.theme_name, settings.overrides);

        Self {
            settings,
            registry,
            resolved_theme,
            user_managed,
            config_path,
            state_path,
        }
    }

    pub fn set_settings(&mut self, settings: Settings) -> Result<()> {
        self.settings = settings;
        self.settings.sanitize();
        self.resolved_theme = self
            .registry
            .resolve(&self.settings.theme_name, self.settings.overrides);

        if !self.user_managed {
            write_kdl(&self.state_path, self.export())?;
        }

        Ok(())
    }

    pub fn export(&self) -> String {
        self.settings.to_kdl_string()
    }

    pub fn export_to(&self, path: &Path) -> Result<()> {
        write_kdl(path, self.export())
    }
}

pub fn parse_settings(text: &str) -> Result<Settings> {
    let document = text.parse::<KdlDocument>()?;
    Ok(Settings::from_document(&document))
}

fn config_path() -> PathBuf {
    dirs::config_dir()
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("glyphst")
        .join("config.kdl")
}

fn themes_path() -> PathBuf {
    dirs::config_dir()
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("glyphst")
        .join("themes")
}

fn state_path() -> PathBuf {
    dirs::state_dir()
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("glyphst")
        .join("config.kdl")
}

fn load_settings(path: &Path) -> Result<Settings> {
    fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))
        .and_then(|text| parse_settings(&text))
}

fn write_kdl(path: &Path, contents: String) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_partial_settings_with_defaults() {
        let settings = parse_settings(
            r##"
theme "one-dark"
zoom {
    default 1.5
}
window {
    opacity 0.85
    appearance "transparent"
}
theme-overrides {
    colors {
        background "#101010"
    }
}
"##,
        )
        .unwrap();

        assert_eq!(settings.behavior.default_zoom, 1.5);
        assert_eq!(settings.behavior.zoom_step, DEFAULT_ZOOM_STEP);
        assert_eq!(settings.behavior.smooth_scrolling, DEFAULT_SMOOTH_SCROLLING);
        assert_eq!(settings.window.opacity, 0.85);
        assert_eq!(
            settings.window.background,
            WindowBackgroundMode::Transparent
        );
        assert_eq!(settings.theme_name, "one-dark");
        assert_eq!(settings.overrides.background, Some(0x101010));
        assert_eq!(settings.overrides.selection_highlight, None);
    }

    #[test]
    fn clamps_window_opacity() {
        let settings = parse_settings(
            r##"
window {
    opacity 0.1
    appearance "blurred"
}
"##,
        )
        .unwrap();

        assert_eq!(settings.window.opacity, MIN_WINDOW_OPACITY);
        assert_eq!(settings.window.background, WindowBackgroundMode::Blurred);
    }

    #[test]
    fn exported_settings_are_reparseable() {
        let settings = Settings {
            behavior: BehaviorSettings {
                default_zoom: 1.5,
                zoom_step: 1.2,
                smooth_scrolling: false,
            },
            theme_name: "catppuccin-mocha".to_string(),
            overrides: PartialTheme {
                background: Some(0x181818),
                selection_highlight: Some(0x61afef66),
                page_gap: Some(32.0),
                ..PartialTheme::default()
            },
            ..Settings::default()
        };

        let exported = settings.clone().to_kdl_string();
        assert_eq!(parse_settings(&exported).unwrap(), settings);
    }

    #[test]
    fn parses_and_exports_keymap_settings() {
        let settings = parse_settings(
            r##"
keymap "vim"
keybindings {
    next-page "j"
    jump-backward "ctrl-o"
}
"##,
        )
        .unwrap();

        assert_eq!(settings.keymap_preset, KeymapPreset::Vim);
        assert_eq!(
            settings.keybinding_overrides,
            vec![
                KeyBindingOverride {
                    action: "next-page".to_string(),
                    keystrokes: "j".to_string(),
                },
                KeyBindingOverride {
                    action: "jump-backward".to_string(),
                    keystrokes: "ctrl-o".to_string(),
                },
            ]
        );

        assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
    }

    #[test]
    fn theme_resolution_layers_selected_theme_and_overrides() {
        let registry = ThemeRegistry::load(Path::new("/path/that/does/not/exist"));
        let settings = Settings {
            theme_name: "one-dark".to_string(),
            overrides: PartialTheme {
                background: Some(0x101010),
                page_gap: Some(48.0),
                ..PartialTheme::default()
            },
            ..Settings::default()
        };

        let theme = registry.resolve(&settings.theme_name, settings.overrides);

        assert_eq!(theme.accent, 0x61afef);
        assert_eq!(theme.background, 0x101010);
        assert_eq!(theme.page_gap, 48.0);
    }

    #[test]
    fn unknown_theme_falls_back_to_default() {
        let registry = ThemeRegistry::load(Path::new("/path/that/does/not/exist"));
        let theme = registry.resolve("missing", PartialTheme::default());

        assert_eq!(theme, Theme::default());
    }

    #[test]
    fn loads_user_theme_from_themes_directory() {
        let root = unique_test_dir();
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("solar.kdl"),
            r##"
name "Solar"
appearance "light"
colors {
    background "#123456"
    accent "#abcdef"
}
page-gap 36
"##,
        )
        .unwrap();

        let registry = ThemeRegistry::load(&root);
        let theme = registry.resolve("solar", PartialTheme::default());
        let metadata = registry
            .metadata()
            .into_iter()
            .find(|metadata| metadata.id == "solar")
            .unwrap();

        assert_eq!(theme.background, 0x123456);
        assert_eq!(theme.accent, 0xabcdef);
        assert_eq!(theme.page_gap, 36.0);
        assert_eq!(metadata.name, "Solar");
        assert_eq!(metadata.appearance.as_str(), "light");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unmanaged_settings_persist_to_state_without_creating_config() {
        let root = unique_test_dir();
        let config_path = root.join("config").join("glyphst").join("config.kdl");
        let state_path = root.join("state").join("glyphst").join("config.kdl");
        let settings = Settings {
            behavior: BehaviorSettings {
                default_zoom: 1.5,
                ..BehaviorSettings::default()
            },
            ..Settings::default()
        };
        let mut store = test_store(
            Settings::default(),
            false,
            config_path.clone(),
            state_path.clone(),
        );

        store.set_settings(settings.clone()).unwrap();

        assert!(!config_path.exists());
        assert_eq!(
            parse_settings(&fs::read_to_string(&state_path).unwrap()).unwrap(),
            settings
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn managed_settings_changes_do_not_touch_state_file() {
        let root = unique_test_dir();
        let config_path = root.join("config").join("glyphst").join("config.kdl");
        let state_path = root.join("state").join("glyphst").join("config.kdl");
        let settings = Settings {
            overrides: PartialTheme {
                page_gap: Some(48.0),
                ..PartialTheme::default()
            },
            ..Settings::default()
        };
        let mut store = test_store(Settings::default(), true, config_path, state_path.clone());

        store.set_settings(settings.clone()).unwrap();

        assert!(!state_path.exists());
        assert_eq!(store.settings, settings);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn export_to_writes_selected_path() {
        let root = unique_test_dir();
        let export_path = root.join("picked").join("glyphst.kdl");
        let settings = Settings {
            behavior: BehaviorSettings {
                default_zoom: 1.5,
                zoom_step: 1.2,
                smooth_scrolling: false,
            },
            theme_name: "glyphst-light".to_string(),
            overrides: PartialTheme {
                background: Some(0x181818),
                selection_highlight: Some(0x61afef66),
                ..PartialTheme::default()
            },
            ..Settings::default()
        };
        let store = test_store(
            settings.clone(),
            false,
            root.join("config.kdl"),
            root.join("state.kdl"),
        );

        store.export_to(&export_path).unwrap();

        assert_eq!(
            parse_settings(&fs::read_to_string(&export_path).unwrap()).unwrap(),
            settings
        );

        fs::remove_dir_all(root).ok();
    }

    fn test_store(
        settings: Settings,
        user_managed: bool,
        config_path: PathBuf,
        state_path: PathBuf,
    ) -> SettingsStore {
        let registry = ThemeRegistry::load(Path::new("/path/that/does/not/exist"));
        let resolved_theme = registry.resolve(&settings.theme_name, settings.overrides);
        SettingsStore {
            settings,
            registry,
            resolved_theme,
            user_managed,
            config_path,
            state_path,
        }
    }

    fn unique_test_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "glyphst-settings-test-{}-{nanos}",
            std::process::id()
        ))
    }
}
