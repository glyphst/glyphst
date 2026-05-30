use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use gpui::Global;
use kdl::{KdlDocument, KdlNode, KdlValue};

pub const DEFAULT_ZOOM: f32 = 1.0;
pub const DEFAULT_ZOOM_STEP: f32 = 1.05;
pub const DEFAULT_PAGE_GAP: f32 = 24.0;
pub const DEFAULT_BACKGROUND: u32 = 0x202020;
pub const DEFAULT_SELECTION_HIGHLIGHT: u32 = 0x61afef66;

const MIN_ZOOM_SETTING: f32 = 0.25;
const MAX_ZOOM_SETTING: f32 = 8.0;
const MIN_ZOOM_STEP: f32 = 1.01;
const MAX_ZOOM_STEP: f32 = 4.0;
const MIN_PAGE_GAP: f32 = 0.0;
const MAX_PAGE_GAP: f32 = 240.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub default_zoom: f32,
    pub zoom_step: f32,
    pub page_gap: f32,
    pub background: u32,
    pub selection_highlight: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_zoom: DEFAULT_ZOOM,
            zoom_step: DEFAULT_ZOOM_STEP,
            page_gap: DEFAULT_PAGE_GAP,
            background: DEFAULT_BACKGROUND,
            selection_highlight: DEFAULT_SELECTION_HIGHLIGHT,
        }
    }
}

impl Settings {
    pub fn sanitize(&mut self) {
        self.default_zoom = self.default_zoom.clamp(MIN_ZOOM_SETTING, MAX_ZOOM_SETTING);
        self.zoom_step = self.zoom_step.clamp(MIN_ZOOM_STEP, MAX_ZOOM_STEP);
        self.page_gap = self.page_gap.clamp(MIN_PAGE_GAP, MAX_PAGE_GAP);
        self.background &= 0x00ff_ffff;
        self.selection_highlight &= 0xffff_ffff;
    }

    fn from_document(document: &KdlDocument) -> Self {
        let mut settings = Self::default();

        if let Some(zoom) = document.get("zoom").and_then(KdlNode::children) {
            if let Some(value) = zoom.get_arg("default").and_then(value_to_f32) {
                settings.default_zoom = value;
            }
            if let Some(value) = zoom.get_arg("step").and_then(value_to_f32) {
                settings.zoom_step = value;
            }
        }

        if let Some(value) = document.get_arg("page-gap").and_then(value_to_f32) {
            settings.page_gap = value;
        }

        if let Some(colors) = document.get("colors").and_then(KdlNode::children) {
            if let Some(value) = colors
                .get_arg("background")
                .and_then(KdlValue::as_string)
                .and_then(|value| parse_hex_color(value, false))
            {
                settings.background = value;
            }
            if let Some(value) = colors
                .get_arg("selection-highlight")
                .and_then(KdlValue::as_string)
                .and_then(|value| parse_hex_color(value, true))
            {
                settings.selection_highlight = value;
            }
        }

        settings.sanitize();
        settings
    }

    pub fn to_kdl_string(self) -> String {
        let mut document = KdlDocument::new();

        let mut zoom = KdlNode::new("zoom");
        let zoom_children = zoom.ensure_children();
        push_value_node(zoom_children, "default", self.default_zoom as f64);
        push_value_node(zoom_children, "step", self.zoom_step as f64);
        document.nodes_mut().push(zoom);

        push_value_node(&mut document, "page-gap", self.page_gap as f64);

        let mut colors = KdlNode::new("colors");
        let color_children = colors.ensure_children();
        push_value_node(
            color_children,
            "background",
            format_rgb_hex(self.background),
        );
        push_value_node(
            color_children,
            "selection-highlight",
            format_rgba_hex(self.selection_highlight),
        );
        document.nodes_mut().push(colors);

        document.autoformat();
        document.to_string()
    }
}

pub struct SettingsStore {
    pub settings: Settings,
    pub user_managed: bool,
    pub config_path: PathBuf,
    pub state_path: PathBuf,
}

impl Global for SettingsStore {}

impl SettingsStore {
    pub fn load() -> Self {
        let config_path = config_path();
        let state_path = state_path();
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

        Self {
            settings,
            user_managed,
            config_path,
            state_path,
        }
    }

    pub fn set_settings(&mut self, settings: Settings) -> Result<()> {
        self.settings = settings;
        self.settings.sanitize();

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

fn push_value_node(document: &mut KdlDocument, name: &str, value: impl Into<KdlValue>) {
    let mut node = KdlNode::new(name);
    node.push(value);
    document.nodes_mut().push(node);
}

fn value_to_f32(value: &KdlValue) -> Option<f32> {
    match value {
        KdlValue::Float(value) if value.is_finite() => Some(*value as f32),
        KdlValue::Integer(value) => Some(*value as f32),
        _ => None,
    }
}

fn parse_hex_color(value: &str, allow_alpha: bool) -> Option<u32> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_partial_settings_with_defaults() {
        let settings = parse_settings(
            r##"
zoom {
    default 1.5
}
colors {
    background "#101010"
}
"##,
        )
        .unwrap();

        assert_eq!(settings.default_zoom, 1.5);
        assert_eq!(settings.zoom_step, DEFAULT_ZOOM_STEP);
        assert_eq!(settings.page_gap, DEFAULT_PAGE_GAP);
        assert_eq!(settings.background, 0x101010);
        assert_eq!(settings.selection_highlight, DEFAULT_SELECTION_HIGHLIGHT);
    }

    #[test]
    fn exported_settings_are_reparseable() {
        let settings = Settings {
            default_zoom: 1.5,
            zoom_step: 1.2,
            page_gap: 32.0,
            background: 0x181818,
            selection_highlight: 0x61afef66,
        };

        let exported = settings.to_kdl_string();
        assert_eq!(parse_settings(&exported).unwrap(), settings);
    }

    #[test]
    fn unmanaged_settings_persist_to_state_without_creating_config() {
        let root = unique_test_dir();
        let config_path = root.join("config").join("glyphst").join("config.kdl");
        let state_path = root.join("state").join("glyphst").join("config.kdl");
        let mut store = SettingsStore {
            settings: Settings::default(),
            user_managed: false,
            config_path: config_path.clone(),
            state_path: state_path.clone(),
        };
        let settings = Settings {
            default_zoom: 1.5,
            ..Settings::default()
        };

        store.set_settings(settings).unwrap();

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
        let mut store = SettingsStore {
            settings: Settings::default(),
            user_managed: true,
            config_path,
            state_path: state_path.clone(),
        };
        let settings = Settings {
            page_gap: 48.0,
            ..Settings::default()
        };

        store.set_settings(settings).unwrap();

        assert!(!state_path.exists());
        assert_eq!(store.settings, settings);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn export_to_writes_selected_path() {
        let root = unique_test_dir();
        let export_path = root.join("picked").join("glyphst.kdl");
        let settings = Settings {
            default_zoom: 1.5,
            zoom_step: 1.2,
            page_gap: 32.0,
            background: 0x181818,
            selection_highlight: 0x61afef66,
        };
        let store = SettingsStore {
            settings,
            user_managed: false,
            config_path: root.join("config.kdl"),
            state_path: root.join("state.kdl"),
        };

        store.export_to(&export_path).unwrap();

        assert_eq!(
            parse_settings(&fs::read_to_string(&export_path).unwrap()).unwrap(),
            settings
        );

        fs::remove_dir_all(root).ok();
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
