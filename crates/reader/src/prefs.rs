//! The app's own preferences, kept as one JSON string in eframe's storage
//! (as pdfcraft does), with the last session following pdfcraft's
//! `last_session.rs`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    book_effects::{self, BookEffects},
    keys::{self, KeymapPreset, Overrides},
    sidebar::SidebarTab,
    theme::{Accent, Density, ThemePreference},
};
use glyphst_ui::fonts::FontChoice;

/// The egui Reader's key before it became the only Reader, so preferences
/// carry over.
const STORAGE_KEY: &str = "glyphst-reader-egui";
/// More files than anyone keeps open; storage is untrusted.
pub const MAX_SESSION_FILES: usize = 64;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub theme: ThemePreference,
    pub accent: Accent,
    pub density: Density,
    /// The chrome's base size in points (`theme::BASE_SCALE` is the
    /// design's).
    pub ui_scale: f32,
    /// Rule titles, tree lines and dot fields; off draws plain labels.
    pub glyph_ornaments: bool,
    /// The chrome and prose families.
    pub fonts: FontChoice,
    pub reopen_last_session: bool,
    /// Effect id of the transition between views, or `None` for a cut.
    pub transition: Option<String>,
    /// Effect id of the background behind pages.
    pub background: String,
    /// Frames per second an animated background may repaint at.
    pub background_fps: u32,
    /// Effect id of the tint over pages, or `None` for none.
    pub overlay: Option<String>,
    /// The image backgrounds with `image: true` draw.
    pub wallpaper: Option<String>,
    /// Parameter values the user set, by effect id (manifest order).
    pub effect_params: BTreeMap<String, [f32; glyphst_effects::MAX_PARAMS]>,
    /// Books' own effects, by document path.
    pub books: BTreeMap<String, BookEffects>,
    pub show_sidebar: bool,
    pub sidebar: SidebarTab,
    /// Widths of the left drawer and the assistant, in points.
    pub drawer_width: f32,
    pub assistant_width: f32,
    /// Fade the dock and the rail while reading.
    pub autohide_controls: bool,
    /// Language to translate into; `None` follows the translation settings.
    pub target_language: Option<String>,
    /// Keyboard shortcuts the user changed: command id to keys.
    pub shortcuts: Overrides,
    /// Which default keys commands have.
    pub keymap: KeymapPreset,
    pub last_session: LastSession,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: ThemePreference::System,
            accent: Accent::default(),
            density: Density::default(),
            ui_scale: glyphst_ui::theme::BASE_SCALE,
            glyph_ornaments: true,
            fonts: FontChoice::default(),
            reopen_last_session: true,
            transition: Some(glyphst_effects::SLIDE_OVER.into()),
            background: glyphst_effects::SOLID.into(),
            background_fps: 30,
            overlay: None,
            wallpaper: None,
            effect_params: BTreeMap::new(),
            books: BTreeMap::new(),
            show_sidebar: true,
            sidebar: SidebarTab::default(),
            drawer_width: 300.0,
            assistant_width: 380.0,
            autohide_controls: true,
            target_language: None,
            shortcuts: Overrides::new(),
            keymap: KeymapPreset::Standard,
            last_session: LastSession::default(),
        }
    }
}

/// The documents open when the app last closed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LastSession {
    pub files: Vec<SessionFile>,
    /// Index into `files` of the active tab; `None` for the home tab.
    pub active: Option<usize>,
    /// The split panes, with indices into `files`.
    #[serde(default)]
    pub panes: Option<craft_ui::docking::Layout<usize>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionFile {
    pub path: String,
    /// The page shown at the top of the view (0-based).
    pub page: usize,
}

impl Prefs {
    /// Reads the preferences; anything missing or malformed falls back to
    /// its default.
    pub fn load(storage: Option<&dyn eframe::Storage>) -> Self {
        let mut prefs: Prefs = storage
            .and_then(|storage| storage.get_string(STORAGE_KEY))
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        prefs.last_session.files.truncate(MAX_SESSION_FILES);
        if prefs
            .last_session
            .active
            .is_some_and(|active| active >= prefs.last_session.files.len())
        {
            prefs.last_session.active = None;
        }
        prefs.background_fps = prefs.background_fps.clamp(1, 120);
        let defaults = Prefs::default();
        for (width, default) in [
            (&mut prefs.drawer_width, defaults.drawer_width),
            (&mut prefs.assistant_width, defaults.assistant_width),
        ] {
            *width = if width.is_finite() {
                width.clamp(crate::stage::DRAWER_MIN, 2000.0)
            } else {
                default
            };
        }
        prefs.shortcuts = keys::sanitize(&prefs.shortcuts);
        prefs
            .effect_params
            .retain(|_, values| values.iter().all(|value| value.is_finite()));
        book_effects::trim(&mut prefs.books);
        prefs
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        if let Ok(text) = serde_json::to_string(self) {
            storage.set_string(STORAGE_KEY, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use eframe::Storage as _;

    use super::*;

    #[derive(Default)]
    struct MemoryStorage(HashMap<String, String>);

    impl eframe::Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.into(), value);
        }
        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }
        fn flush(&mut self) {}
    }

    #[test]
    fn preferences_round_trip_with_the_last_session() {
        let mut storage = MemoryStorage::default();
        let prefs = Prefs {
            theme: ThemePreference::Dark,
            transition: None,
            show_sidebar: false,
            sidebar: SidebarTab::Notes,
            drawer_width: 340.0,
            assistant_width: 420.0,
            autohide_controls: false,
            target_language: Some("ja".into()),
            shortcuts: [("text.find".to_string(), vec!["Ctrl+G".to_string()])].into(),
            keymap: KeymapPreset::Vim,
            overlay: Some("sepia".into()),
            wallpaper: Some("/pictures/lake.jpg".into()),
            effect_params: [("aurora".to_string(), [1.5, 0.2, 0.0, 0.0])].into(),
            books: [(
                "/books/a.pdf".to_string(),
                BookEffects {
                    background: crate::book_effects::Choice::Effect("aurora".into()),
                    overlay: crate::book_effects::Choice::Off,
                    used: 3,
                    ..Default::default()
                },
            )]
            .into(),
            last_session: LastSession {
                files: vec![
                    SessionFile {
                        path: "/books/a.pdf".into(),
                        page: 12,
                    },
                    SessionFile {
                        path: "/books/b.epub".into(),
                        page: 0,
                    },
                ],
                active: Some(1),
                panes: Some(craft_ui::docking::Layout {
                    root: Some(craft_ui::docking::Node::Split {
                        axis: craft_ui::layout::SplitAxis::Horizontal,
                        size: craft_ui::layout::SplitSize::Ratio(0.4),
                        first: Box::new(craft_ui::docking::Node::Tabs {
                            panels: vec![0],
                            active: 0,
                        }),
                        second: Box::new(craft_ui::docking::Node::Tabs {
                            panels: vec![1],
                            active: 0,
                        }),
                    }),
                    floating: Vec::new(),
                }),
            },
            ..Prefs::default()
        };
        prefs.save(&mut storage);
        assert_eq!(Prefs::load(Some(&storage)), prefs);
    }

    #[test]
    fn untrusted_storage_is_sanitized() {
        let mut storage = MemoryStorage::default();
        storage.set_string(
            STORAGE_KEY,
            r#"{"theme":"Light","background_fps":0,"drawer_width":1e9,"shortcuts":{"text.find":["Nope"],"gone":["F2"]},"last_session":{"files":[],"active":3},"unknown":1}"#.into(),
        );
        let prefs = Prefs::load(Some(&storage));
        assert_eq!(prefs.theme, ThemePreference::Light);
        assert_eq!(prefs.last_session.active, None);
        assert_eq!(prefs.background_fps, 1);
        assert_eq!(prefs.drawer_width, 2000.0);
        assert!(
            prefs.shortcuts.is_empty(),
            "unknown commands and keys are dropped"
        );
        assert!(prefs.reopen_last_session, "missing fields take defaults");
        assert_eq!(prefs.keymap, KeymapPreset::Standard);

        storage.set_string(STORAGE_KEY, "not json".into());
        assert_eq!(Prefs::load(Some(&storage)), Prefs::default());
        assert_eq!(Prefs::load(None), Prefs::default());
    }
}
