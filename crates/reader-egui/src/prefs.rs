//! The app's own preferences, kept as one JSON string in eframe's storage
//! (as pdfcraft does), with the last session following pdfcraft's
//! `last_session.rs`.

use serde::{Deserialize, Serialize};

use crate::{sidebar::SidebarTab, theme::ThemePreference};

const STORAGE_KEY: &str = "glyphst-reader-egui";
/// More files than anyone keeps open; storage is untrusted.
pub const MAX_SESSION_FILES: usize = 64;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub theme: ThemePreference,
    pub reopen_last_session: bool,
    /// Effect id of the transition between views, or `None` for a cut.
    pub transition: Option<String>,
    /// Effect id of the background behind pages.
    pub background: String,
    /// Frames per second an animated background may repaint at.
    pub background_fps: u32,
    pub show_sidebar: bool,
    pub sidebar: SidebarTab,
    pub last_session: LastSession,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: ThemePreference::System,
            reopen_last_session: true,
            transition: Some(glyphst_effects::SLIDE_OVER.into()),
            background: glyphst_effects::SOLID.into(),
            background_fps: 30,
            show_sidebar: true,
            sidebar: SidebarTab::default(),
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
            r#"{"theme":"Light","background_fps":0,"last_session":{"files":[],"active":3},"unknown":1}"#.into(),
        );
        let prefs = Prefs::load(Some(&storage));
        assert_eq!(prefs.theme, ThemePreference::Light);
        assert_eq!(prefs.last_session.active, None);
        assert_eq!(prefs.background_fps, 1);
        assert!(prefs.reopen_last_session, "missing fields take defaults");

        storage.set_string(STORAGE_KEY, "not json".into());
        assert_eq!(Prefs::load(Some(&storage)), Prefs::default());
        assert_eq!(Prefs::load(None), Prefs::default());
    }
}
