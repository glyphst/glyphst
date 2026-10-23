//! The app's own preferences, kept as one JSON string in eframe's storage
//! (as pdfcraft does), with the last session following pdfcraft's
//! `last_session.rs`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    book_effects::{self, BookEffects},
    keys::{self, KeymapPreset, Overrides},
    present::{self, RingSlots},
    sidebar::SidebarTab,
    theme::{Accent, Density, ThemeKind, ThemePreference},
};
use glyphst_ui::{
    fonts::FontChoice,
    scheme::{Contrast, SchemeStyle},
};

/// The egui Reader's key before it became the only Reader, so preferences
/// carry over.
const STORAGE_KEY: &str = "glyphst-reader-egui";
/// More files than anyone keeps open; storage is untrusted.
pub const MAX_SESSION_FILES: usize = 64;
/// The longest talk the clock counts down.
pub const MAX_TALK_MINUTES: u32 = 600;
/// Page counts remembered for home.
pub const MAX_PAGE_COUNTS: usize = 2_000;

/// How many columns of thumbnails the Pages sidebar shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageColumns {
    One,
    #[default]
    Two,
}

impl PageColumns {
    pub fn count(self) -> usize {
        match self {
            Self::One => 1,
            Self::Two => 2,
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::One => Self::Two,
            Self::Two => Self::One,
        }
    }
}

/// Where the theme's colours come from (Settings › Appearance ›
/// colours).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Colours {
    /// Follow the background: a scheme from its solid colour or the
    /// wallpaper's main colour, else (the theme's colour, an effect) the
    /// accent swatch.
    #[default]
    Background,
    /// The accent swatch over the design's neutrals.
    Preset,
    /// A scheme from `custom_colour`.
    Custom,
}

/// Whether opening a document adds it to the library.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AddOnOpen {
    Off,
    /// Added where it is: its history and highlights stay with its path.
    #[default]
    InPlace,
}

/// How dropped and imported files come in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportMode {
    /// Copied into the library folder.
    #[default]
    Copy,
    /// Left where they are.
    Link,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryPrefs {
    pub add_on_open: AddOnOpen,
    pub import_mode: ImportMode,
    /// Look metadata up in arXiv, Crossref, OpenAlex and Open Library.
    pub look_up_online: bool,
    /// Sent to Crossref (its polite pool) when set; nothing else is.
    pub contact_email: String,
    /// The library model summarises, tags and files each item added.
    pub enrich_new: bool,
    /// The AI may make up to two new tags an item; otherwise it only uses
    /// tags the library has.
    pub ai_create_tags: bool,
    /// The AI may propose new collections (never makes them itself).
    pub suggest_collections: bool,
    /// The AI files an item into an existing collection it is sure of;
    /// otherwise it only proposes it. No item has to be in one.
    pub auto_file_collections: bool,
    /// The vault's folder when it is not the library folder's `notes/`
    /// (an existing Obsidian vault, say).
    pub vault: Option<String>,
    /// The library tab asked where the library should live (frame 04k).
    pub folder_asked: bool,
    /// Reading the library's text for search waits until resumed.
    pub index_paused: bool,
    /// The user's own `llama-server` for search by meaning; empty: the
    /// one downloaded with the model.
    pub llama_server: String,
    /// The search results stop offering search by meaning (Settings still
    /// does).
    pub meaning_offer_hidden: bool,
    /// The reranker rereads each search's best passages (once downloaded).
    pub rerank: bool,
    /// Searches and the passages opened from them are kept on this
    /// computer, to measure search by.
    pub learn_from_opened: bool,
    /// How the library syncs with this person's other computers.
    pub sync: SyncMode,
    /// The WebDAV server or S3 bucket it syncs through (the secret is in
    /// the keychain).
    pub sync_remote: SyncRemotePrefs,
    /// The library table's columns the user turned off.
    pub hidden_columns: Vec<crate::library_tab::columns::Column>,
}

/// Where the library syncs when it is not through a sync tool.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncRemotePrefs {
    pub s3_endpoint: String,
    /// `auto` for Cloudflare R2; the bucket's region elsewhere.
    pub s3_region: String,
    pub s3_bucket: String,
    /// A folder in the bucket, or empty.
    pub s3_prefix: String,
    pub s3_key_id: String,
    /// The library's folder on the server.
    pub webdav_url: String,
    pub webdav_user: String,
}

/// Where the library's changes go to reach other computers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncMode {
    #[default]
    Off,
    /// Through the library folder, which a sync tool (Syncthing, iCloud,
    /// Dropbox) carries.
    Folder,
    /// Mirrored to a WebDAV server (Nextcloud, a NAS).
    WebDav,
    /// Mirrored to an S3 bucket (Cloudflare R2, Backblaze B2).
    S3,
}

impl LibraryPrefs {
    /// The vault in use: the folder set here, else `notes/` in the library
    /// folder once the user chose one; none until then.
    pub fn vault_root(
        &self,
        files_root: Option<&std::path::Path>,
        chosen: bool,
    ) -> Option<std::path::PathBuf> {
        match &self.vault {
            Some(vault) if !vault.trim().is_empty() => Some(std::path::PathBuf::from(vault.trim())),
            _ => files_root.filter(|_| chosen).map(|root| root.join("notes")),
        }
    }
}

impl Default for LibraryPrefs {
    fn default() -> Self {
        Self {
            add_on_open: AddOnOpen::default(),
            import_mode: ImportMode::default(),
            look_up_online: true,
            contact_email: String::new(),
            enrich_new: true,
            index_paused: false,
            llama_server: String::new(),
            meaning_offer_hidden: false,
            rerank: false,
            learn_from_opened: true,
            ai_create_tags: true,
            suggest_collections: true,
            auto_file_collections: false,
            vault: None,
            folder_asked: false,
            sync: SyncMode::Off,
            sync_remote: SyncRemotePrefs::default(),
            hidden_columns: Vec::new(),
        }
    }
}

/// The colour a wallpaper's scheme is built on, found once per image.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WallpaperSeed {
    pub path: String,
    pub seed: [u8; 3],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub theme: ThemePreference,
    pub accent: Accent,
    pub colours: Colours,
    /// The seed of `Colours::Custom`, sRGB.
    pub custom_colour: [u8; 3],
    /// How a generated scheme builds on its seed.
    pub scheme_style: SchemeStyle,
    pub scheme_contrast: Contrast,
    /// The chosen wallpaper's seed, kept so the theme needn't wait for the
    /// image to load.
    pub wallpaper_seed: Option<WallpaperSeed>,
    pub density: Density,
    /// The chrome's base size in points (`theme::BASE_SCALE` is the
    /// design's).
    pub ui_scale: f32,
    /// Rule titles, tree lines and dot fields; off draws plain labels.
    pub glyph_ornaments: bool,
    /// The chrome and prose families.
    pub fonts: FontChoice,
    /// Documents open at the zoom they were left at; off, at fit width
    /// (on the page they were left at either way).
    pub remember_zoom: bool,
    pub reopen_last_session: bool,
    /// Keys and jumps glide the view to where they go.
    pub smooth_scrolling: bool,
    /// Motion becomes short fades: slides and glides jump, pages fade in.
    pub reduce_motion: bool,
    /// A ring closes on where a jump lands (back, a link, the outline, a
    /// search hit).
    pub jump_ring: bool,
    /// Effect id of the page turn's transition (presenting), or `None` for
    /// a cut.
    pub transition: Option<String>,
    /// Effect id of the transition between books, [`glyphst_effects::RANDOM`]
    /// for a different one each switch, or `None` for a cut.
    pub switch_transition: Option<String>,
    /// How the book switcher lays out its cards.
    pub switcher_layout: crate::switcher::layout::Layout,
    /// Effect id of the background behind pages.
    pub background: String,
    /// The animated background last chosen, restored when Appearance's
    /// background goes back to animated.
    pub animated_background: Option<String>,
    /// Frames per second an animated background may repaint at.
    pub background_fps: u32,
    /// The page tint before it was the theme's: moved to both tints when
    /// read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overlay: Option<String>,
    /// Effect ids of the tint over pages in the light and the dark theme,
    /// or `None` for none.
    pub tint_light: Option<String>,
    pub tint_dark: Option<String>,
    /// The image backgrounds with `image: true` draw.
    pub wallpaper: Option<String>,
    /// Parameter values the user set, by effect id (manifest order).
    pub effect_params: BTreeMap<String, [f32; glyphst_effects::MAX_PARAMS]>,
    /// Books' own effects, by document path.
    pub books: BTreeMap<String, BookEffects>,
    pub show_sidebar: bool,
    pub sidebar: SidebarTab,
    /// Columns of thumbnails in the Pages sidebar.
    pub page_columns: PageColumns,
    /// The outline scrolls to follow the section being read.
    pub outline_follow: bool,
    /// Widths of the left drawer and the assistant, in points.
    pub drawer_width: f32,
    pub assistant_width: f32,
    /// Language to translate into; `None` follows the translation settings.
    pub target_language: Option<String>,
    /// Page counts of documents opened before, by path, for home's
    /// progress.
    pub page_counts: BTreeMap<String, usize>,
    /// How documents come into the library and what the AI does with them
    /// (Settings › Library).
    pub library: LibraryPrefs,
    /// While translated pages show, the original shows under the pointer.
    pub original_on_hover: bool,
    /// Opening a PDF starts translating it.
    pub translate_as_read: bool,
    /// Keyboard shortcuts the user changed: command id to keys.
    pub shortcuts: Overrides,
    /// Which default keys commands have.
    pub keymap: KeymapPreset,
    /// Gamepad buttons the user changed: command id to buttons.
    pub pad_shortcuts: Overrides,
    /// Presenting makes the window fullscreen (and leaving puts it back).
    pub present_fullscreen: bool,
    /// The page number shows in the presentation's corner (the timer
    /// tool adds the time).
    pub present_page_number: bool,
    /// The talk's length in minutes, for the clock to count down; 0
    /// counts up.
    pub talk_minutes: u32,
    /// The tool ring's commands by id, from up clockwise.
    pub present_ring: RingSlots,
    /// The reading ring's slots (the right stick opens it).
    pub reading_ring: RingSlots,
    /// Gamepad mode (frames 09c–j): the fullscreen shell for reading with
    /// a pad from afar.
    pub gamepad_mode: bool,
    /// A pad connecting asks whether to switch to gamepad mode.
    pub ask_gamepad_mode: bool,
    /// Gamepad mode's text size, as zoom over the design's.
    pub couch_text: f32,
    pub last_session: LastSession,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: ThemePreference::System,
            accent: Accent::default(),
            colours: Colours::default(),
            custom_colour: [0x5B, 0x7D, 0xB1],
            scheme_style: SchemeStyle::default(),
            scheme_contrast: Contrast::default(),
            wallpaper_seed: None,
            density: Density::default(),
            ui_scale: glyphst_ui::theme::BASE_SCALE,
            glyph_ornaments: true,
            fonts: FontChoice::default(),
            remember_zoom: true,
            reopen_last_session: true,
            smooth_scrolling: true,
            reduce_motion: false,
            jump_ring: true,
            transition: Some(glyphst_effects::SLIDE_OVER.into()),
            switch_transition: Some(glyphst_effects::RANDOM.into()),
            switcher_layout: crate::switcher::layout::Layout::Fan,
            background: glyphst_effects::SOLID.into(),
            animated_background: None,
            background_fps: 30,
            overlay: None,
            tint_light: None,
            tint_dark: None,
            wallpaper: None,
            effect_params: BTreeMap::new(),
            books: BTreeMap::new(),
            show_sidebar: true,
            sidebar: SidebarTab::default(),
            page_columns: PageColumns::default(),
            outline_follow: true,
            drawer_width: crate::shell::SIDEBAR_WIDTH,
            assistant_width: crate::shell::ASSISTANT_WIDTH,
            target_language: None,
            page_counts: BTreeMap::new(),
            library: LibraryPrefs::default(),
            original_on_hover: true,
            translate_as_read: false,
            shortcuts: Overrides::new(),
            keymap: KeymapPreset::Standard,
            pad_shortcuts: Overrides::new(),
            present_fullscreen: true,
            present_page_number: false,
            talk_minutes: 0,
            present_ring: present::default_ring(),
            reading_ring: present::default_reading_ring(),
            gamepad_mode: false,
            ask_gamepad_mode: true,
            couch_text: 1.0,
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
    pub panes: Option<glyphst_ui::craft::docking::Layout<usize>>,
    /// The tabs that are not documents, in the bar's order.
    pub pages: Vec<SessionPage>,
    /// Index into `pages` of the one shown, when one was.
    pub page_focused: Option<usize>,
}

/// A tab that is not a document, as it was left.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SessionPage {
    Library {
        view: crate::library_tab::View,
        filter: String,
        selected: Option<glyphst_library::DocumentId>,
    },
    Keymap,
    Activity,
    NewTab,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionFile {
    pub path: String,
    /// The page shown at the top of the view (0-based).
    pub page: usize,
    #[serde(default)]
    pub pinned: bool,
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
        prefs.talk_minutes = prefs.talk_minutes.min(MAX_TALK_MINUTES);
        // Storage is untrusted: keep the newest few hundred counts.
        while prefs.page_counts.len() > MAX_PAGE_COUNTS {
            let first = prefs.page_counts.keys().next().cloned();
            if let Some(first) = first {
                prefs.page_counts.remove(&first);
            }
        }
        let scale = glyphst_ui::theme::UI_SCALE_RANGE;
        prefs.ui_scale = if prefs.ui_scale.is_finite() {
            prefs.ui_scale.clamp(*scale.start(), *scale.end())
        } else {
            glyphst_ui::theme::BASE_SCALE
        };
        let defaults = Prefs::default();
        for (width, default) in [
            (&mut prefs.drawer_width, defaults.drawer_width),
            (&mut prefs.assistant_width, defaults.assistant_width),
        ] {
            *width = if width.is_finite() {
                width.clamp(crate::shell::SIDEBAR_MIN, 2000.0)
            } else {
                default
            };
        }
        prefs.shortcuts = keys::sanitize(&prefs.shortcuts);
        prefs.pad_shortcuts = crate::pad_keys::sanitize(&prefs.pad_shortcuts);
        prefs.couch_text = crate::couch::step_text_size(prefs.couch_text, 0);
        prefs
            .effect_params
            .retain(|_, values| values.iter().all(|value| value.is_finite()));
        prefs.merge_wallpaper_effects();
        if let Some(overlay) = prefs.overlay.take() {
            prefs.tint_light = Some(overlay.clone());
            prefs.tint_dark = Some(overlay);
        }
        book_effects::trim(&mut prefs.books);
        prefs
    }

    /// The page tint of the `kind` theme.
    pub fn tint(&self, kind: ThemeKind) -> Option<&str> {
        match kind {
            ThemeKind::Light => self.tint_light.as_deref(),
            ThemeKind::Dark => self.tint_dark.as_deref(),
        }
    }

    pub fn tint_mut(&mut self, kind: ThemeKind) -> &mut Option<String> {
        match kind {
            ThemeKind::Light => &mut self.tint_light,
            ThemeKind::Dark => &mut self.tint_dark,
        }
    }

    /// The soft and parallax wallpapers became settings of the one
    /// wallpaper effect (dim, blur, parallax, depth): their choices and
    /// values move to it.
    fn merge_wallpaper_effects(&mut self) {
        const SOFT: &str = "image-soft";
        const PARALLAX: &str = "image-parallax";
        let image = glyphst_effects::IMAGE;
        let soft = self.effect_params.remove(SOFT);
        let parallax = self.effect_params.remove(PARALLAX);
        // Old values in each one's manifest order, else its defaults.
        let merged = match self.background.as_str() {
            SOFT => {
                let [blur, dim, ..] = soft.unwrap_or([4.0, 0.35, 0.0, 0.0]);
                Some([dim, blur, 0.0, 0.03])
            }
            PARALLAX => {
                let [depth, blur, ..] = parallax.unwrap_or([0.03, 1.0, 0.0, 0.0]);
                Some([0.0, blur, 1.0, depth])
            }
            _ => None,
        };
        if let Some(values) = merged {
            self.background = image.into();
            self.effect_params.insert(image.into(), values);
        }
        for book in self.books.values_mut() {
            if let book_effects::Choice::Effect(id) = &mut book.background
                && (id == SOFT || id == PARALLAX)
            {
                *id = image.into();
            }
        }
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
    fn a_session_from_before_page_tabs_still_loads() {
        let session: LastSession =
            serde_json::from_str(r#"{"files":[{"path":"/a.pdf","page":3}],"active":0}"#).unwrap();
        assert_eq!(session.files.len(), 1);
        assert!(session.pages.is_empty());
        assert_eq!(session.page_focused, None);
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
            target_language: Some("ja".into()),
            shortcuts: [("text.find".to_string(), vec!["Ctrl+G".to_string()])].into(),
            keymap: KeymapPreset::Vim,
            tint_light: Some("sepia".into()),
            tint_dark: Some("night".into()),
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
                        pinned: true,
                    },
                    SessionFile {
                        path: "/books/b.epub".into(),
                        page: 0,
                        pinned: false,
                    },
                ],
                active: Some(1),
                panes: Some(glyphst_ui::craft::docking::Layout {
                    root: Some(glyphst_ui::craft::docking::Node::Split {
                        axis: glyphst_ui::craft::layout::SplitAxis::Horizontal,
                        size: glyphst_ui::craft::layout::SplitSize::Ratio(0.4),
                        first: Box::new(glyphst_ui::craft::docking::Node::Tabs {
                            panels: vec![0],
                            active: 0,
                        }),
                        second: Box::new(glyphst_ui::craft::docking::Node::Tabs {
                            panels: vec![1],
                            active: 0,
                        }),
                    }),
                    floating: Vec::new(),
                }),
                pages: vec![
                    SessionPage::Library {
                        view: crate::library_tab::View::Tag(glyphst_library::LibraryTagId(7)),
                        filter: "year:>2020".into(),
                        selected: Some(glyphst_library::DocumentId(3)),
                    },
                    SessionPage::Keymap,
                ],
                page_focused: Some(0),
            },
            ..Prefs::default()
        };
        prefs.save(&mut storage);
        assert_eq!(Prefs::load(Some(&storage)), prefs);
    }

    #[test]
    fn appearance_choices_round_trip_and_the_scale_stays_in_range() {
        let mut storage = MemoryStorage::default();
        let prefs = Prefs {
            accent: Accent::Green,
            colours: Colours::Custom,
            custom_colour: [1, 2, 3],
            scheme_style: SchemeStyle::Expressive,
            scheme_contrast: Contrast::High,
            wallpaper_seed: Some(WallpaperSeed {
                path: "/w.jpg".into(),
                seed: [9, 8, 7],
            }),
            density: Density::Compact,
            ui_scale: 15.0,
            glyph_ornaments: false,
            fonts: FontChoice {
                chrome: "Iosevka".into(),
                prose: "Inter".into(),
            },
            original_on_hover: false,
            translate_as_read: true,
            ..Prefs::default()
        };
        prefs.save(&mut storage);
        assert_eq!(Prefs::load(Some(&storage)), prefs);
        // The theme's colours follow the background unless chosen.
        assert_eq!(Prefs::default().colours, Colours::Background);
        let huge = Prefs {
            ui_scale: 400.0,
            ..Prefs::default()
        };
        huge.save(&mut storage);
        assert_eq!(
            Prefs::load(Some(&storage)).ui_scale,
            *glyphst_ui::theme::UI_SCALE_RANGE.end()
        );
    }

    #[test]
    fn soft_and_parallax_wallpapers_become_the_wallpaper_with_their_values() {
        let load = |json: &str| {
            let mut storage = MemoryStorage::default();
            storage.set_string(STORAGE_KEY, json.into());
            Prefs::load(Some(&storage))
        };
        let soft = load(
            r#"{"background":"image-soft","effect_params":{"image-soft":[6,0.5,0,0]},
                "books":{"a.pdf":{"background":{"effect":"image-parallax"}}}}"#,
        );
        assert_eq!(soft.background, glyphst_effects::IMAGE);
        assert_eq!(
            soft.effect_params[glyphst_effects::IMAGE],
            [0.5, 6.0, 0.0, 0.03]
        );
        assert!(!soft.effect_params.contains_key("image-soft"));
        assert_eq!(
            soft.books["a.pdf"].background,
            book_effects::Choice::Effect(glyphst_effects::IMAGE.into())
        );
        let parallax = load(r#"{"background":"image-parallax"}"#);
        assert_eq!(parallax.background, glyphst_effects::IMAGE);
        assert_eq!(
            parallax.effect_params[glyphst_effects::IMAGE],
            [0.0, 1.0, 1.0, 0.03]
        );
        // The plain wallpaper's dim keeps its place.
        let plain = load(r#"{"background":"image","effect_params":{"image":[0.4,0,0,0]}}"#);
        assert_eq!(plain.effect_params[glyphst_effects::IMAGE][0], 0.4);
    }

    #[test]
    fn the_page_tint_of_before_becomes_both_themes_tint() {
        let mut storage = MemoryStorage::default();
        storage.set_string(STORAGE_KEY, r#"{"overlay":"sepia"}"#.into());
        let prefs = Prefs::load(Some(&storage));
        assert_eq!(prefs.tint(ThemeKind::Light), Some("sepia"));
        assert_eq!(prefs.tint(ThemeKind::Dark), Some("sepia"));
        assert_eq!(prefs.overlay, None);
        prefs.save(&mut storage);
        assert!(
            !storage
                .get_string(STORAGE_KEY)
                .unwrap()
                .contains("\"overlay\"")
        );
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
        assert!(prefs.smooth_scrolling && !prefs.reduce_motion);
        assert_eq!(prefs.keymap, KeymapPreset::Standard);

        storage.set_string(STORAGE_KEY, "not json".into());
        assert_eq!(Prefs::load(Some(&storage)), Prefs::default());
        assert_eq!(Prefs::load(None), Prefs::default());
    }
}
