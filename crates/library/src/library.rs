use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, anyhow};
pub use glyphst_core::{EpubReaderPreferences, HighlightColor, SemanticTextAnchor};
use gpui::Global;
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

const MAX_HISTORY_ENTRIES: usize = 100;
// Legacy tables are retained so pre-v1 databases remain readable.
const HISTORY_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("history");
const BOOKMARKS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("bookmarks");
const HIGHLIGHTS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("highlights");
const NOTES_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("notes");
const AI_SESSIONS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("ai_sessions");
const UI_STATE_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("ui_state");

const STORAGE_SCHEMA_VERSION: u8 = 1;
const STORAGE_MIGRATIONS_TABLE: TableDefinition<&str, &[u8]> =
    TableDefinition::new("storage_migrations");
const STORAGE_V1_MIGRATION_KEY: &str = "storage_v1";
const HISTORY_V1_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("history_v1");
const BOOKMARKS_V1_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("bookmarks_v1");
const HIGHLIGHTS_V1_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("highlights_v1");
const NOTES_V1_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("notes_v1");
const AI_SESSIONS_V1_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("ai_sessions_v1");
const UI_STATE_V1_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("ui_state_v1");
const DOCUMENT_PREFERENCES_V1_TABLE: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("document_preferences_v1");
const STORAGE_V2_MIGRATION_KEY: &str = "storage_v2";
const STORAGE_V2_VERSION: u8 = 2;
const HISTORY_V2_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("history_v2");
const BOOKMARKS_V2_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("bookmarks_v2");
const HIGHLIGHTS_V2_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("highlights_v2");
const NOTES_V2_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("notes_v2");
const DOCUMENT_PREFERENCES_V2_TABLE: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("document_preferences_v2");
const SIDEBAR_WIDTHS_KEY: &str = "sidebar_widths";
const MAX_AI_SESSIONS: usize = 200;

#[derive(Clone, Debug, Serialize, Deserialize)]
enum StoredPathV1 {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}

impl StoredPathV1 {
    fn from_path(path: &Path) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;

            Self::Unix(path.as_os_str().as_bytes().to_vec())
        }

        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt as _;

            Self::Windows(path.as_os_str().encode_wide().collect())
        }
    }

    fn into_path(self) -> Result<PathBuf> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt as _;

            match self {
                Self::Unix(bytes) => Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes))),
                Self::Windows(_) => Err(anyhow!("Windows path found in a Unix library database")),
            }
        }

        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt as _;

            match self {
                Self::Unix(_) => Err(anyhow!("Unix path found in a Windows library database")),
                Self::Windows(units) => Ok(PathBuf::from(std::ffi::OsString::from_wide(&units))),
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct StoredDocumentPositionV1 {
    scroll_x_doc: f32,
    scroll_y_doc: f32,
    zoom: f32,
    page: usize,
}

impl From<&DocumentPosition> for StoredDocumentPositionV1 {
    fn from(position: &DocumentPosition) -> Self {
        Self {
            scroll_x_doc: position.scroll_x_doc,
            scroll_y_doc: position.scroll_y_doc,
            zoom: position.zoom,
            page: position.page,
        }
    }
}

impl StoredDocumentPositionV1 {
    fn into_domain(self) -> DocumentPosition {
        DocumentPosition {
            scroll_x_doc: self.scroll_x_doc,
            scroll_y_doc: self.scroll_y_doc,
            zoom: self.zoom,
            page: self.page,
            semantic: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LegacyHistoryEntry {
    path: PathBuf,
    opened: u64,
    position: StoredDocumentPositionV1,
}

impl LegacyHistoryEntry {
    fn into_domain(self) -> HistoryEntry {
        HistoryEntry {
            path: self.path,
            opened: self.opened,
            position: self.position.into_domain(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LegacyBookmark {
    id: u64,
    path: PathBuf,
    label: String,
    position: StoredDocumentPositionV1,
    created: u64,
}

impl LegacyBookmark {
    fn into_domain(self) -> Bookmark {
        Bookmark {
            id: self.id,
            path: self.path,
            label: self.label,
            position: self.position.into_domain(),
            created: self.created,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LegacyHighlight {
    id: u64,
    path: PathBuf,
    color: HighlightColor,
    rects: Vec<HighlightRect>,
    text: String,
    note: String,
    created: u64,
}

impl LegacyHighlight {
    fn into_domain(self) -> Highlight {
        Highlight {
            id: self.id,
            path: self.path,
            color: self.color,
            rects: self.rects,
            semantic_range: None,
            text: self.text,
            note: self.note,
            created: self.created,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LegacyNote {
    id: u64,
    path: PathBuf,
    color: HighlightColor,
    rects: Vec<HighlightRect>,
    text: String,
    body: String,
    created: u64,
}

impl LegacyNote {
    fn into_domain(self) -> Note {
        Note {
            id: self.id,
            path: self.path,
            color: self.color,
            rects: self.rects,
            semantic_range: None,
            text: self.text,
            body: self.body,
            created: self.created,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredHistoryEntryV1 {
    version: u8,
    path: StoredPathV1,
    opened: u64,
    position: StoredDocumentPositionV1,
}

impl From<&HistoryEntry> for StoredHistoryEntryV1 {
    fn from(entry: &HistoryEntry) -> Self {
        Self {
            version: STORAGE_SCHEMA_VERSION,
            path: StoredPathV1::from_path(&entry.path),
            opened: entry.opened,
            position: StoredDocumentPositionV1::from(&entry.position),
        }
    }
}

impl StoredHistoryEntryV1 {
    fn into_domain(self) -> Result<HistoryEntry> {
        require_storage_version(self.version, STORAGE_SCHEMA_VERSION)?;
        Ok(HistoryEntry {
            path: self.path.into_path()?,
            opened: self.opened,
            position: self.position.into_domain(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredBookmarkV1 {
    version: u8,
    id: u64,
    path: StoredPathV1,
    label: String,
    position: StoredDocumentPositionV1,
    created: u64,
}

impl From<&Bookmark> for StoredBookmarkV1 {
    fn from(bookmark: &Bookmark) -> Self {
        Self {
            version: STORAGE_SCHEMA_VERSION,
            id: bookmark.id,
            path: StoredPathV1::from_path(&bookmark.path),
            label: bookmark.label.clone(),
            position: StoredDocumentPositionV1::from(&bookmark.position),
            created: bookmark.created,
        }
    }
}

impl StoredBookmarkV1 {
    fn into_domain(self) -> Result<Bookmark> {
        require_storage_version(self.version, STORAGE_SCHEMA_VERSION)?;
        Ok(Bookmark {
            id: self.id,
            path: self.path.into_path()?,
            label: self.label,
            position: self.position.into_domain(),
            created: self.created,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredHighlightV1 {
    version: u8,
    id: u64,
    path: StoredPathV1,
    color: HighlightColor,
    rects: Vec<HighlightRect>,
    text: String,
    note: String,
    created: u64,
}

impl From<&Highlight> for StoredHighlightV1 {
    fn from(highlight: &Highlight) -> Self {
        Self {
            version: STORAGE_SCHEMA_VERSION,
            id: highlight.id,
            path: StoredPathV1::from_path(&highlight.path),
            color: highlight.color,
            rects: highlight.rects.clone(),
            text: highlight.text.clone(),
            note: highlight.note.clone(),
            created: highlight.created,
        }
    }
}

impl StoredHighlightV1 {
    fn into_domain(self) -> Result<Highlight> {
        require_storage_version(self.version, STORAGE_SCHEMA_VERSION)?;
        Ok(Highlight {
            id: self.id,
            path: self.path.into_path()?,
            color: self.color,
            rects: self.rects,
            semantic_range: None,
            text: self.text,
            note: self.note,
            created: self.created,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredNoteV1 {
    version: u8,
    id: u64,
    path: StoredPathV1,
    color: HighlightColor,
    rects: Vec<HighlightRect>,
    text: String,
    body: String,
    created: u64,
}

impl From<&Note> for StoredNoteV1 {
    fn from(note: &Note) -> Self {
        Self {
            version: STORAGE_SCHEMA_VERSION,
            id: note.id,
            path: StoredPathV1::from_path(&note.path),
            color: note.color,
            rects: note.rects.clone(),
            text: note.text.clone(),
            body: note.body.clone(),
            created: note.created,
        }
    }
}

impl StoredNoteV1 {
    fn into_domain(self) -> Result<Note> {
        require_storage_version(self.version, STORAGE_SCHEMA_VERSION)?;
        Ok(Note {
            id: self.id,
            path: self.path.into_path()?,
            color: self.color,
            rects: self.rects,
            semantic_range: None,
            text: self.text,
            body: self.body,
            created: self.created,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredAiSessionV1 {
    version: u8,
    id: u64,
    path: StoredPathV1,
    title: String,
    page: usize,
    messages: Vec<AiSessionMessage>,
    created: u64,
    updated: u64,
}

impl From<&AiSession> for StoredAiSessionV1 {
    fn from(session: &AiSession) -> Self {
        Self {
            version: STORAGE_SCHEMA_VERSION,
            id: session.id,
            path: StoredPathV1::from_path(&session.path),
            title: session.title.clone(),
            page: session.page,
            messages: session.messages.clone(),
            created: session.created,
            updated: session.updated,
        }
    }
}

impl StoredAiSessionV1 {
    fn into_domain(self) -> Result<AiSession> {
        require_storage_version(self.version, STORAGE_SCHEMA_VERSION)?;
        Ok(AiSession {
            id: self.id,
            path: self.path.into_path()?,
            title: self.title,
            page: self.page,
            messages: self.messages,
            created: self.created,
            updated: self.updated,
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct StoredSidebarWidthsV1 {
    version: u8,
    widths: SidebarWidths,
}

impl From<SidebarWidths> for StoredSidebarWidthsV1 {
    fn from(widths: SidebarWidths) -> Self {
        Self {
            version: STORAGE_SCHEMA_VERSION,
            widths,
        }
    }
}

impl StoredSidebarWidthsV1 {
    fn into_domain(self) -> Result<SidebarWidths> {
        require_storage_version(self.version, STORAGE_SCHEMA_VERSION)?;
        Ok(self.widths)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredDocumentPreferencesV1 {
    version: u8,
    path: StoredPathV1,
    target_language: String,
}

impl From<&DocumentPreferences> for StoredDocumentPreferencesV1 {
    fn from(preferences: &DocumentPreferences) -> Self {
        Self {
            version: STORAGE_SCHEMA_VERSION,
            path: StoredPathV1::from_path(&preferences.path),
            target_language: preferences.target_language.clone(),
        }
    }
}

impl StoredDocumentPreferencesV1 {
    fn into_domain(self) -> Result<DocumentPreferences> {
        require_storage_version(self.version, STORAGE_SCHEMA_VERSION)?;
        Ok(DocumentPreferences {
            path: self.path.into_path()?,
            target_language: self.target_language,
            epub: None,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredHistoryEntryV2 {
    version: u8,
    path: StoredPathV1,
    opened: u64,
    position: DocumentPosition,
}

impl From<&HistoryEntry> for StoredHistoryEntryV2 {
    fn from(entry: &HistoryEntry) -> Self {
        Self {
            version: STORAGE_V2_VERSION,
            path: StoredPathV1::from_path(&entry.path),
            opened: entry.opened,
            position: entry.position.clone(),
        }
    }
}

impl StoredHistoryEntryV2 {
    fn into_domain(self) -> Result<HistoryEntry> {
        require_storage_version(self.version, STORAGE_V2_VERSION)?;
        Ok(HistoryEntry {
            path: self.path.into_path()?,
            opened: self.opened,
            position: self.position,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredBookmarkV2 {
    version: u8,
    id: u64,
    path: StoredPathV1,
    label: String,
    position: DocumentPosition,
    created: u64,
}

impl From<&Bookmark> for StoredBookmarkV2 {
    fn from(bookmark: &Bookmark) -> Self {
        Self {
            version: STORAGE_V2_VERSION,
            id: bookmark.id,
            path: StoredPathV1::from_path(&bookmark.path),
            label: bookmark.label.clone(),
            position: bookmark.position.clone(),
            created: bookmark.created,
        }
    }
}

impl StoredBookmarkV2 {
    fn into_domain(self) -> Result<Bookmark> {
        require_storage_version(self.version, STORAGE_V2_VERSION)?;
        Ok(Bookmark {
            id: self.id,
            path: self.path.into_path()?,
            label: self.label,
            position: self.position,
            created: self.created,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredHighlightV2 {
    version: u8,
    id: u64,
    path: StoredPathV1,
    color: HighlightColor,
    rects: Vec<HighlightRect>,
    semantic_range: Option<SemanticTextRange>,
    text: String,
    note: String,
    created: u64,
}

impl From<&Highlight> for StoredHighlightV2 {
    fn from(highlight: &Highlight) -> Self {
        Self {
            version: STORAGE_V2_VERSION,
            id: highlight.id,
            path: StoredPathV1::from_path(&highlight.path),
            color: highlight.color,
            rects: highlight.rects.clone(),
            semantic_range: highlight.semantic_range.clone(),
            text: highlight.text.clone(),
            note: highlight.note.clone(),
            created: highlight.created,
        }
    }
}

impl StoredHighlightV2 {
    fn into_domain(self) -> Result<Highlight> {
        require_storage_version(self.version, STORAGE_V2_VERSION)?;
        Ok(Highlight {
            id: self.id,
            path: self.path.into_path()?,
            color: self.color,
            rects: self.rects,
            semantic_range: self.semantic_range,
            text: self.text,
            note: self.note,
            created: self.created,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredNoteV2 {
    version: u8,
    id: u64,
    path: StoredPathV1,
    color: HighlightColor,
    rects: Vec<HighlightRect>,
    semantic_range: Option<SemanticTextRange>,
    text: String,
    body: String,
    created: u64,
}

impl From<&Note> for StoredNoteV2 {
    fn from(note: &Note) -> Self {
        Self {
            version: STORAGE_V2_VERSION,
            id: note.id,
            path: StoredPathV1::from_path(&note.path),
            color: note.color,
            rects: note.rects.clone(),
            semantic_range: note.semantic_range.clone(),
            text: note.text.clone(),
            body: note.body.clone(),
            created: note.created,
        }
    }
}

impl StoredNoteV2 {
    fn into_domain(self) -> Result<Note> {
        require_storage_version(self.version, STORAGE_V2_VERSION)?;
        Ok(Note {
            id: self.id,
            path: self.path.into_path()?,
            color: self.color,
            rects: self.rects,
            semantic_range: self.semantic_range,
            text: self.text,
            body: self.body,
            created: self.created,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredDocumentPreferencesV2 {
    version: u8,
    path: StoredPathV1,
    target_language: String,
    epub: Option<EpubReaderPreferences>,
}

impl From<&DocumentPreferences> for StoredDocumentPreferencesV2 {
    fn from(preferences: &DocumentPreferences) -> Self {
        Self {
            version: STORAGE_V2_VERSION,
            path: StoredPathV1::from_path(&preferences.path),
            target_language: preferences.target_language.clone(),
            epub: preferences.epub,
        }
    }
}

impl StoredDocumentPreferencesV2 {
    fn into_domain(self) -> Result<DocumentPreferences> {
        require_storage_version(self.version, STORAGE_V2_VERSION)?;
        Ok(DocumentPreferences {
            path: self.path.into_path()?,
            target_language: self.target_language,
            epub: self.epub,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentPosition {
    pub scroll_x_doc: f32,
    pub scroll_y_doc: f32,
    pub zoom: f32,
    pub page: usize,
    pub semantic: Option<ReflowPosition>,
}

impl DocumentPosition {
    pub fn fixed(scroll_x_doc: f32, scroll_y_doc: f32, zoom: f32, page: usize) -> Self {
        Self {
            scroll_x_doc,
            scroll_y_doc,
            zoom,
            page,
            semantic: None,
        }
    }

    pub fn reflow(
        anchor: SemanticTextAnchor,
        viewport_fraction: f32,
        progress: f32,
        zoom: f32,
        generated_page: usize,
    ) -> Self {
        Self {
            scroll_x_doc: 0.0,
            scroll_y_doc: 0.0,
            zoom,
            page: generated_page,
            semantic: Some(ReflowPosition {
                anchor,
                viewport_fraction: finite_fraction(viewport_fraction),
                progress: finite_fraction(progress),
            }),
        }
    }

    pub fn is_reflow(&self) -> bool {
        self.semantic.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReflowPosition {
    pub anchor: SemanticTextAnchor,
    pub viewport_fraction: f32,
    pub progress: f32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticTextRange {
    pub start: SemanticTextAnchor,
    pub end: SemanticTextAnchor,
}

struct AnnotationGeometry {
    rects: Vec<HighlightRect>,
    semantic_range: Option<SemanticTextRange>,
}

impl AnnotationGeometry {
    fn fixed(rects: Vec<HighlightRect>) -> Self {
        Self {
            rects,
            semantic_range: None,
        }
    }

    fn reflow(rects: Vec<HighlightRect>, semantic_range: SemanticTextRange) -> Self {
        Self {
            rects,
            semantic_range: Some(semantic_range),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub path: PathBuf,
    pub opened: u64,
    pub position: DocumentPosition,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: u64,
    pub path: PathBuf,
    pub label: String,
    pub position: DocumentPosition,
    pub created: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HighlightRect {
    pub page_idx: usize,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl HighlightRect {
    pub fn contains(&self, page_idx: usize, x: f32, y: f32) -> bool {
        self.page_idx == page_idx && x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Highlight {
    pub id: u64,
    pub path: PathBuf,
    pub color: HighlightColor,
    pub rects: Vec<HighlightRect>,
    pub semantic_range: Option<SemanticTextRange>,
    pub text: String,
    pub note: String,
    pub created: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: u64,
    pub path: PathBuf,
    pub color: HighlightColor,
    pub rects: Vec<HighlightRect>,
    pub semantic_range: Option<SemanticTextRange>,
    pub text: String,
    pub body: String,
    pub created: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiSessionRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiSessionMessage {
    pub role: AiSessionRole,
    pub content: String,
    pub request_content: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiSession {
    pub id: u64,
    pub path: PathBuf,
    pub title: String,
    pub page: usize,
    pub messages: Vec<AiSessionMessage>,
    pub created: u64,
    pub updated: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SidebarWidths {
    pub thumbnails: f32,
    pub toc: f32,
    pub notes: f32,
    pub ai: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentPreferences {
    pub path: PathBuf,
    pub target_language: String,
    pub epub: Option<EpubReaderPreferences>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Library {
    pub history: Vec<HistoryEntry>,
    pub bookmarks: Vec<Bookmark>,
    pub highlights: Vec<Highlight>,
    pub notes: Vec<Note>,
    pub ai_sessions: Vec<AiSession>,
    pub sidebar_widths: Option<SidebarWidths>,
    pub document_preferences: Vec<DocumentPreferences>,
    next_bookmark_id: u64,
    next_highlight_id: u64,
    next_note_id: u64,
    next_ai_session_id: u64,
}

pub struct LibraryStore {
    pub library: Library,
    database: Option<Database>,
    startup_error: Option<String>,
}

impl Global for LibraryStore {}

impl LibraryStore {
    pub fn load() -> Self {
        let path = library_path();
        match Self::open(path) {
            Ok(store) => store,
            Err(error) => {
                eprintln!("failed to open document library database: {error:#}");
                Self {
                    library: Library::default(),
                    database: None,
                    startup_error: Some(format!("{error:#}")),
                }
            }
        }
    }

    fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            create_private_directory(parent)?;
        }

        let database = if path.exists() {
            Database::open(&path).with_context(|| format!("failed to open {}", path.display()))?
        } else {
            Database::create(&path)
                .with_context(|| format!("failed to create {}", path.display()))?
        };
        restrict_library_file(&path)?;
        ensure_tables(&database)?;
        let library = load_library(&database)?;

        let store = Self {
            library,
            database: Some(database),
            startup_error: None,
        };
        if let Err(error) = store.prune_ai_session_rows() {
            eprintln!("failed to prune expired AI sessions: {error:#}");
        }
        Ok(store)
    }

    pub fn startup_error(&self) -> Option<&str> {
        self.startup_error.as_deref()
    }

    pub fn history_entries(&self) -> &[HistoryEntry] {
        &self.library.history
    }

    pub fn sidebar_widths(&self) -> Option<SidebarWidths> {
        self.library.sidebar_widths
    }

    pub fn set_sidebar_widths(&mut self, widths: SidebarWidths) -> Result<()> {
        self.write_sidebar_widths(widths)?;
        self.library.sidebar_widths = Some(widths);
        Ok(())
    }

    pub fn document_target_language(&self, path: &Path) -> Option<String> {
        let path = normalize_path(path);
        self.library
            .document_preferences
            .iter()
            .find(|preferences| preferences.path == path)
            .map(|preferences| preferences.target_language.clone())
            .filter(|target_language| !target_language.is_empty())
    }

    pub fn epub_reader_preferences(&self, path: &Path) -> Option<EpubReaderPreferences> {
        let path = normalize_path(path);
        self.library
            .document_preferences
            .iter()
            .find(|preferences| preferences.path == path)
            .and_then(|preferences| preferences.epub)
    }

    pub fn set_document_target_language(
        &mut self,
        path: &Path,
        target_language: impl Into<String>,
    ) -> Result<()> {
        let path = normalize_path(path);
        let target_language = target_language.into().trim().to_ascii_lowercase();
        if target_language.is_empty() {
            return Err(anyhow!("document target language cannot be empty"));
        }
        let epub = self
            .library
            .document_preferences
            .iter()
            .find(|preferences| preferences.path == path)
            .and_then(|preferences| preferences.epub);
        let preferences = DocumentPreferences {
            path: path.clone(),
            target_language,
            epub,
        };
        self.write_document_preferences(&preferences)?;
        if let Some(existing) = self
            .library
            .document_preferences
            .iter_mut()
            .find(|existing| existing.path == path)
        {
            *existing = preferences;
        } else {
            self.library.document_preferences.push(preferences);
            self.library
                .document_preferences
                .sort_by(|left, right| left.path.cmp(&right.path));
        }
        Ok(())
    }

    pub fn set_epub_reader_preferences(
        &mut self,
        path: &Path,
        preferences: EpubReaderPreferences,
    ) -> Result<()> {
        let path = normalize_path(path);
        let preferences = preferences.sanitized();
        let target_language = self
            .library
            .document_preferences
            .iter()
            .find(|existing| existing.path == path)
            .map(|existing| existing.target_language.clone())
            .unwrap_or_default();
        let document_preferences = DocumentPreferences {
            path: path.clone(),
            target_language,
            epub: Some(preferences),
        };
        self.write_document_preferences(&document_preferences)?;
        if let Some(existing) = self
            .library
            .document_preferences
            .iter_mut()
            .find(|existing| existing.path == path)
        {
            *existing = document_preferences;
        } else {
            self.library.document_preferences.push(document_preferences);
            self.library
                .document_preferences
                .sort_by(|left, right| left.path.cmp(&right.path));
        }
        Ok(())
    }

    pub fn clear_epub_reader_preferences(&mut self, path: &Path) -> Result<()> {
        let path = normalize_path(path);
        let Some(index) = self
            .library
            .document_preferences
            .iter()
            .position(|preferences| preferences.path == path)
        else {
            return Ok(());
        };
        if self.library.document_preferences[index].epub.is_none() {
            return Ok(());
        }

        let mut preferences = self.library.document_preferences[index].clone();
        preferences.epub = None;
        if preferences.target_language.is_empty() {
            self.delete_document_preferences(&path)?;
            self.library.document_preferences.remove(index);
            return Ok(());
        }
        self.write_document_preferences(&preferences)?;
        self.library.document_preferences[index] = preferences;
        Ok(())
    }

    pub fn remove_missing_history_entries(&mut self) -> Result<bool> {
        let mut history = self.library.history.clone();
        history.retain(|entry| entry.path.exists());
        if history.len() == self.library.history.len() {
            return Ok(false);
        }

        self.write_history_entries(&history)?;
        self.library.history = history;
        Ok(true)
    }

    pub fn bookmarks(&self) -> &[Bookmark] {
        &self.library.bookmarks
    }

    pub fn highlights_for(&self, path: &Path) -> Vec<Highlight> {
        let path = normalize_path(path);
        self.library
            .highlights
            .iter()
            .filter(|highlight| highlight.path == path)
            .cloned()
            .collect()
    }

    pub fn notes_for(&self, path: &Path) -> Vec<Note> {
        let path = normalize_path(path);
        self.library
            .notes
            .iter()
            .filter(|note| note.path == path)
            .cloned()
            .collect()
    }

    pub fn ai_sessions_for(&self, path: &Path) -> Vec<AiSession> {
        let path = normalize_path(path);
        self.library
            .ai_sessions
            .iter()
            .filter(|session| session.path == path)
            .cloned()
            .collect()
    }

    pub fn next_ai_session_id(&mut self) -> u64 {
        let id = self.library.next_ai_session_id.max(1);
        self.library.next_ai_session_id = id + 1;
        id
    }

    pub fn history_position(&self, path: &Path) -> Option<DocumentPosition> {
        let path = normalize_path(path);
        self.library
            .history
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.position.clone())
    }

    pub fn record_history(&mut self, path: &Path, position: DocumentPosition) {
        self.library
            .record_history_at(path, position, current_timestamp());
    }

    pub fn remove_history_entries(&mut self, paths: &[PathBuf]) -> Result<usize> {
        let path_set: HashSet<PathBuf> = paths.iter().map(|p| normalize_path(p)).collect();
        let mut history = self.library.history.clone();
        history.retain(|entry| !path_set.contains(&entry.path));
        let removed = self.library.history.len() - history.len();
        if removed > 0 {
            self.write_history_entries(&history)?;
            self.library.history = history;
        }
        Ok(removed)
    }

    pub fn add_bookmark(
        &mut self,
        path: &Path,
        label: impl Into<String>,
        position: DocumentPosition,
    ) -> Result<u64> {
        let bookmark = self
            .library
            .build_bookmark(path, label, position, current_timestamp());
        self.write_bookmark(&bookmark)?;
        self.library.bookmarks.push(bookmark.clone());
        self.library.sanitize();
        Ok(bookmark.id)
    }

    pub fn remove_bookmark(&mut self, id: u64) -> Result<bool> {
        if !self
            .library
            .bookmarks
            .iter()
            .any(|bookmark| bookmark.id == id)
        {
            return Ok(false);
        }

        self.remove_bookmark_from_database(id)?;
        Ok(self.library.remove_bookmark(id))
    }

    pub fn add_highlight(
        &mut self,
        path: &Path,
        color: HighlightColor,
        rects: Vec<HighlightRect>,
        text: impl Into<String>,
        note: impl Into<String>,
    ) -> Result<Highlight> {
        let highlight = self.library.build_highlight(
            path,
            color,
            AnnotationGeometry::fixed(rects),
            text,
            note,
            current_timestamp(),
        );
        self.write_highlight(&highlight)?;
        self.library.highlights.push(highlight.clone());
        self.library.sanitize();
        Ok(highlight)
    }

    pub fn add_reflow_highlight(
        &mut self,
        path: &Path,
        color: HighlightColor,
        semantic_range: SemanticTextRange,
        rects: Vec<HighlightRect>,
        text: impl Into<String>,
        note: impl Into<String>,
    ) -> Result<Highlight> {
        let highlight = self.library.build_highlight(
            path,
            color,
            AnnotationGeometry::reflow(rects, semantic_range),
            text,
            note,
            current_timestamp(),
        );
        self.write_highlight(&highlight)?;
        self.library.highlights.push(highlight.clone());
        self.library.sanitize();
        Ok(highlight)
    }

    pub fn add_note(
        &mut self,
        path: &Path,
        color: HighlightColor,
        rects: Vec<HighlightRect>,
        text: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Note> {
        let note = self.library.build_note(
            path,
            color,
            AnnotationGeometry::fixed(rects),
            text,
            body,
            current_timestamp(),
        );
        self.write_note(&note)?;
        self.library.notes.push(note.clone());
        self.library.sanitize();
        Ok(note)
    }

    pub fn add_reflow_note(
        &mut self,
        path: &Path,
        color: HighlightColor,
        semantic_range: SemanticTextRange,
        rects: Vec<HighlightRect>,
        text: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Note> {
        let note = self.library.build_note(
            path,
            color,
            AnnotationGeometry::reflow(rects, semantic_range),
            text,
            body,
            current_timestamp(),
        );
        self.write_note(&note)?;
        self.library.notes.push(note.clone());
        self.library.sanitize();
        Ok(note)
    }

    pub fn remove_highlight(&mut self, id: u64) -> Result<bool> {
        if !self
            .library
            .highlights
            .iter()
            .any(|highlight| highlight.id == id)
        {
            return Ok(false);
        }

        self.remove_highlight_from_database(id)?;
        Ok(self.library.remove_highlight(id))
    }

    pub fn remove_note(&mut self, id: u64) -> Result<bool> {
        if !self.library.notes.iter().any(|note| note.id == id) {
            return Ok(false);
        }

        self.remove_note_from_database(id)?;
        Ok(self.library.remove_note(id))
    }

    pub fn set_highlight_color(&mut self, id: u64, color: HighlightColor) -> Result<bool> {
        let Some(highlight_ix) = self
            .library
            .highlights
            .iter()
            .position(|highlight| highlight.id == id)
        else {
            return Ok(false);
        };

        let mut highlight = self.library.highlights[highlight_ix].clone();
        highlight.color = color;
        self.write_highlight(&highlight)?;
        self.library.highlights[highlight_ix] = highlight;
        Ok(true)
    }

    pub fn update_note(
        &mut self,
        id: u64,
        body: impl Into<String>,
        color: HighlightColor,
    ) -> Result<bool> {
        let Some(note_ix) = self.library.notes.iter().position(|note| note.id == id) else {
            return Ok(false);
        };

        let mut note = self.library.notes[note_ix].clone();
        note.body = body.into();
        note.color = color;
        self.write_note(&note)?;
        self.library.notes[note_ix] = note;
        self.library.sanitize();
        Ok(true)
    }

    pub fn save_ai_session(&mut self, mut session: AiSession) -> Result<()> {
        session.path = normalize_path(&session.path);
        session.title = session.title.trim().to_string();
        if session.title.is_empty() {
            session.title = "New Chat".to_string();
        }
        let session_id = session.id;

        let mut library = self.library.clone();
        if let Some(session_ix) = library
            .ai_sessions
            .iter()
            .position(|existing| existing.id == session_id)
        {
            library.ai_sessions[session_ix] = session;
        } else {
            library.ai_sessions.push(session);
        }
        library.sanitize();

        let session = library
            .ai_sessions
            .iter()
            .find(|existing| existing.id == session_id)
            .ok_or_else(|| anyhow!("saved AI session is invalid"))?;
        let retained_ids = library
            .ai_sessions
            .iter()
            .map(|existing| existing.id)
            .collect::<HashSet<_>>();
        let removed_ids = self
            .library
            .ai_sessions
            .iter()
            .map(|existing| existing.id)
            .filter(|id| !retained_ids.contains(id))
            .collect::<Vec<_>>();

        self.write_ai_session_changes(session, &removed_ids)?;
        self.library = library;
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        self.write_history()
    }

    fn write_history(&self) -> Result<()> {
        self.write_history_entries(&self.library.history)
    }

    fn write_history_entries(&self, history: &[HistoryEntry]) -> Result<()> {
        let Some(database) = &self.database else {
            return Err(anyhow!("document library database is unavailable"));
        };
        let valid_paths = history
            .iter()
            .map(|entry| history_key(&entry.path))
            .collect::<HashSet<_>>();
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(HISTORY_V2_TABLE)?;
            let mut existing_paths = Vec::new();
            for entry in table.iter()? {
                let (path, _) = entry?;
                existing_paths.push(path.value().to_vec());
            }

            for path in existing_paths {
                if !valid_paths.contains(&path) {
                    table.remove(path.as_slice())?;
                }
            }

            for entry in history {
                let key = history_key(&entry.path);
                let encoded = encode(&StoredHistoryEntryV2::from(entry))?;
                table.insert(key.as_slice(), encoded.as_slice())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    fn write_bookmark(&self, bookmark: &Bookmark) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(BOOKMARKS_V2_TABLE)?;
            let encoded = encode(&StoredBookmarkV2::from(bookmark))?;
            table.insert(bookmark.id, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn remove_bookmark_from_database(&self, id: u64) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(BOOKMARKS_V2_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn write_highlight(&self, highlight: &Highlight) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(HIGHLIGHTS_V2_TABLE)?;
            let encoded = encode(&StoredHighlightV2::from(highlight))?;
            table.insert(highlight.id, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn remove_highlight_from_database(&self, id: u64) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(HIGHLIGHTS_V2_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn write_note(&self, note: &Note) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(NOTES_V2_TABLE)?;
            let encoded = encode(&StoredNoteV2::from(note))?;
            table.insert(note.id, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn remove_note_from_database(&self, id: u64) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(NOTES_V2_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn write_ai_session_changes(&self, session: &AiSession, removed_ids: &[u64]) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(AI_SESSIONS_V1_TABLE)?;
            let encoded = encode(&StoredAiSessionV1::from(session))?;
            table.insert(session.id, encoded.as_slice())?;
            for id in removed_ids {
                table.remove(*id)?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    fn prune_ai_session_rows(&self) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let retained_ids = self
            .library
            .ai_sessions
            .iter()
            .map(|session| session.id)
            .collect::<HashSet<_>>();
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(AI_SESSIONS_V1_TABLE)?;
            let stale_ids = table
                .iter()?
                .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
                .filter(|id| !retained_ids.contains(id))
                .collect::<Vec<_>>();
            for id in stale_ids {
                table.remove(id)?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    fn write_sidebar_widths(&self, widths: SidebarWidths) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(UI_STATE_V1_TABLE)?;
            let encoded = encode(&StoredSidebarWidthsV1::from(widths))?;
            table.insert(SIDEBAR_WIDTHS_KEY, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn write_document_preferences(&self, preferences: &DocumentPreferences) -> Result<()> {
        let Some(database) = &self.database else {
            return Err(anyhow!("document library database is unavailable"));
        };
        let key = history_key(&preferences.path);
        let encoded = encode(&StoredDocumentPreferencesV2::from(preferences))?;
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V2_TABLE)?;
            table.insert(key.as_slice(), encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn delete_document_preferences(&self, path: &Path) -> Result<()> {
        let Some(database) = &self.database else {
            return Err(anyhow!("document library database is unavailable"));
        };
        let key = history_key(path);
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V2_TABLE)?;
            table.remove(key.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }
}

fn create_private_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

        let already_exists = path.exists();
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        if !already_exists {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("failed to restrict {}", path.display()))?;
        }
    }

    #[cfg(not(unix))]
    {
        fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    }

    Ok(())
}

fn restrict_library_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to restrict {}", path.display()))?;
    }

    Ok(())
}

impl Library {
    fn record_history_at(&mut self, path: &Path, position: DocumentPosition, opened: u64) {
        let path = normalize_path(path);
        self.history.retain(|entry| entry.path != path);
        self.history.push(HistoryEntry {
            path,
            opened,
            position,
        });
        self.sanitize();
    }

    fn build_bookmark(
        &mut self,
        path: &Path,
        label: impl Into<String>,
        position: DocumentPosition,
        created: u64,
    ) -> Bookmark {
        let id = self.next_bookmark_id.max(1);
        self.next_bookmark_id = id + 1;
        Bookmark {
            id,
            path: normalize_path(path),
            label: label.into(),
            position,
            created,
        }
    }

    fn build_highlight(
        &mut self,
        path: &Path,
        color: HighlightColor,
        geometry: AnnotationGeometry,
        text: impl Into<String>,
        note: impl Into<String>,
        created: u64,
    ) -> Highlight {
        let id = self.next_highlight_id.max(1);
        self.next_highlight_id = id + 1;
        Highlight {
            id,
            path: normalize_path(path),
            color,
            rects: geometry.rects,
            semantic_range: geometry.semantic_range,
            text: text.into(),
            note: note.into(),
            created,
        }
    }

    fn build_note(
        &mut self,
        path: &Path,
        color: HighlightColor,
        geometry: AnnotationGeometry,
        text: impl Into<String>,
        body: impl Into<String>,
        created: u64,
    ) -> Note {
        let id = self.next_note_id.max(1);
        self.next_note_id = id + 1;
        Note {
            id,
            path: normalize_path(path),
            color,
            rects: geometry.rects,
            semantic_range: geometry.semantic_range,
            text: text.into(),
            body: body.into(),
            created,
        }
    }

    fn remove_bookmark(&mut self, id: u64) -> bool {
        let before = self.bookmarks.len();
        self.bookmarks.retain(|bookmark| bookmark.id != id);
        before != self.bookmarks.len()
    }

    fn remove_highlight(&mut self, id: u64) -> bool {
        let before = self.highlights.len();
        self.highlights.retain(|highlight| highlight.id != id);
        before != self.highlights.len()
    }

    fn remove_note(&mut self, id: u64) -> bool {
        let before = self.notes.len();
        self.notes.retain(|note| note.id != id);
        before != self.notes.len()
    }

    fn sanitize(&mut self) {
        self.history
            .retain(|entry| !entry.path.as_os_str().is_empty());
        self.history.sort_by(|left, right| {
            right
                .opened
                .cmp(&left.opened)
                .then_with(|| left.path.cmp(&right.path))
        });
        self.history.dedup_by(|left, right| left.path == right.path);
        self.history.truncate(MAX_HISTORY_ENTRIES);

        self.bookmarks.retain_mut(|bookmark| {
            bookmark.label = bookmark.label.trim().to_string();
            !bookmark.path.as_os_str().is_empty() && !bookmark.label.is_empty()
        });
        self.bookmarks.sort_by(|left, right| {
            right
                .created
                .cmp(&left.created)
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.id.cmp(&right.id))
        });

        self.highlights.retain_mut(|highlight| {
            highlight.text = highlight.text.trim().to_string();
            highlight.note = highlight.note.trim().to_string();
            !highlight.path.as_os_str().is_empty()
                && (!highlight.rects.is_empty()
                    || highlight
                        .semantic_range
                        .as_ref()
                        .is_some_and(valid_semantic_range))
        });
        self.highlights.sort_by(|left, right| {
            left.created
                .cmp(&right.created)
                .then_with(|| left.id.cmp(&right.id))
        });

        self.notes.retain_mut(|note| {
            note.text = note.text.trim().to_string();
            note.body = note.body.trim().to_string();
            !note.path.as_os_str().is_empty()
                && (!note.rects.is_empty()
                    || note
                        .semantic_range
                        .as_ref()
                        .is_some_and(valid_semantic_range))
                && !note.body.is_empty()
        });
        self.notes.sort_by(|left, right| {
            left.created
                .cmp(&right.created)
                .then_with(|| left.id.cmp(&right.id))
        });

        self.ai_sessions.retain_mut(|session| {
            session.path = normalize_path(&session.path);
            session.title = session.title.trim().to_string();
            if session.title.is_empty() {
                session.title = "New Chat".to_string();
            }
            session.messages.retain_mut(|message| {
                message.content = message.content.trim().to_string();
                message.request_content = message
                    .request_content
                    .take()
                    .map(|content| content.trim().to_string())
                    .filter(|content| !content.is_empty() && *content != message.content);
                !message.content.is_empty()
            });
            !session.path.as_os_str().is_empty()
                && session.id > 0
                && !session.messages.is_empty()
                && session.created > 0
                && session.updated > 0
        });
        self.ai_sessions.sort_by(|left, right| {
            right
                .updated
                .cmp(&left.updated)
                .then_with(|| right.created.cmp(&left.created))
                .then_with(|| left.id.cmp(&right.id))
        });
        self.ai_sessions.dedup_by(|left, right| left.id == right.id);
        self.ai_sessions.truncate(MAX_AI_SESSIONS);

        self.document_preferences.retain_mut(|preferences| {
            preferences.path = normalize_path(&preferences.path);
            preferences.target_language = preferences.target_language.trim().to_ascii_lowercase();
            preferences.epub = preferences.epub.map(EpubReaderPreferences::sanitized);
            !preferences.path.as_os_str().is_empty()
                && (!preferences.target_language.is_empty() || preferences.epub.is_some())
        });
        self.document_preferences
            .sort_by(|left, right| left.path.cmp(&right.path));
        self.document_preferences
            .dedup_by(|left, right| left.path == right.path);

        self.next_bookmark_id = self.next_bookmark_id.max(
            self.bookmarks
                .iter()
                .map(|bookmark| bookmark.id)
                .max()
                .unwrap_or(0)
                + 1,
        );
        self.next_highlight_id = self.next_highlight_id.max(
            self.highlights
                .iter()
                .map(|highlight| highlight.id)
                .max()
                .unwrap_or(0)
                + 1,
        );
        self.next_note_id = self
            .next_note_id
            .max(self.notes.iter().map(|note| note.id).max().unwrap_or(0) + 1);
        self.next_ai_session_id = self.next_ai_session_id.max(
            self.ai_sessions
                .iter()
                .map(|session| session.id)
                .max()
                .unwrap_or(0)
                + 1,
        );
    }
}

pub fn normalize_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    })
}

fn library_path() -> PathBuf {
    glyphst_core::paths::state_dir().join("library.redb")
}

fn ensure_tables(database: &Database) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        write_txn.open_table(HISTORY_TABLE)?;
    }
    {
        write_txn.open_table(BOOKMARKS_TABLE)?;
    }
    {
        write_txn.open_table(HIGHLIGHTS_TABLE)?;
    }
    {
        write_txn.open_table(NOTES_TABLE)?;
    }
    {
        write_txn.open_table(AI_SESSIONS_TABLE)?;
    }
    {
        write_txn.open_table(UI_STATE_TABLE)?;
    }
    {
        write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
    }
    {
        write_txn.open_table(HISTORY_V1_TABLE)?;
    }
    {
        write_txn.open_table(BOOKMARKS_V1_TABLE)?;
    }
    {
        write_txn.open_table(HIGHLIGHTS_V1_TABLE)?;
    }
    {
        write_txn.open_table(NOTES_V1_TABLE)?;
    }
    {
        write_txn.open_table(AI_SESSIONS_V1_TABLE)?;
    }
    {
        write_txn.open_table(UI_STATE_V1_TABLE)?;
    }
    {
        write_txn.open_table(DOCUMENT_PREFERENCES_V1_TABLE)?;
    }
    {
        write_txn.open_table(HISTORY_V2_TABLE)?;
    }
    {
        write_txn.open_table(BOOKMARKS_V2_TABLE)?;
    }
    {
        write_txn.open_table(HIGHLIGHTS_V2_TABLE)?;
    }
    {
        write_txn.open_table(NOTES_V2_TABLE)?;
    }
    {
        write_txn.open_table(DOCUMENT_PREFERENCES_V2_TABLE)?;
    }
    write_txn.commit()?;
    Ok(())
}

fn load_library(database: &Database) -> Result<Library> {
    let mut library = Library::default();
    let (has_v1_storage, has_v2_storage) = {
        let read_txn = database.begin_read()?;
        let table = read_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        (
            table.get(STORAGE_V1_MIGRATION_KEY)?.is_some(),
            table.get(STORAGE_V2_MIGRATION_KEY)?.is_some(),
        )
    };

    let read_txn = database.begin_read()?;
    if has_v2_storage {
        load_storage_v2(&read_txn, &mut library)?;
    } else if has_v1_storage {
        load_storage_v1(&read_txn, &mut library)?;
    } else {
        load_legacy_storage(&read_txn, &mut library)?;
    }
    drop(read_txn);

    library.sanitize();
    if !has_v1_storage {
        migrate_legacy_storage_v1(database, &library)?;
    }
    if !has_v2_storage {
        migrate_storage_v2(database, &library)?;
    }
    Ok(library)
}

fn load_storage_v2(read_txn: &redb::ReadTransaction, library: &mut Library) -> Result<()> {
    {
        let table = read_txn.open_table(HISTORY_V2_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredHistoryEntryV2>(value.value())
                .and_then(StoredHistoryEntryV2::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.history.push(entry);
                }
                Err(error) => eprintln!("failed to decode document history entry: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(BOOKMARKS_V2_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredBookmarkV2>(value.value()).and_then(StoredBookmarkV2::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.bookmarks.push(entry);
                }
                Err(error) => eprintln!("failed to decode document bookmark: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(HIGHLIGHTS_V2_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredHighlightV2>(value.value())
                .and_then(StoredHighlightV2::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.highlights.push(entry);
                }
                Err(error) => eprintln!("failed to decode document highlight: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(NOTES_V2_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredNoteV2>(value.value()).and_then(StoredNoteV2::into_domain) {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.notes.push(entry);
                }
                Err(error) => eprintln!("failed to decode document note: {error:#}"),
            }
        }
    }
    load_shared_v1_storage(read_txn, library)?;
    {
        let table = read_txn.open_table(DOCUMENT_PREFERENCES_V2_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredDocumentPreferencesV2>(value.value())
                .and_then(StoredDocumentPreferencesV2::into_domain)
            {
                Ok(mut preferences) => {
                    preferences.path = normalize_path(&preferences.path);
                    library.document_preferences.push(preferences);
                }
                Err(error) => eprintln!("failed to decode document preferences: {error:#}"),
            }
        }
    }
    Ok(())
}

fn load_storage_v1(read_txn: &redb::ReadTransaction, library: &mut Library) -> Result<()> {
    {
        let table = read_txn.open_table(HISTORY_V1_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredHistoryEntryV1>(value.value())
                .and_then(StoredHistoryEntryV1::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.history.push(entry);
                }
                Err(error) => eprintln!("failed to decode PDF history entry: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(BOOKMARKS_V1_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredBookmarkV1>(value.value()).and_then(StoredBookmarkV1::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.bookmarks.push(entry);
                }
                Err(error) => eprintln!("failed to decode PDF bookmark: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(HIGHLIGHTS_V1_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredHighlightV1>(value.value())
                .and_then(StoredHighlightV1::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.highlights.push(entry);
                }
                Err(error) => eprintln!("failed to decode PDF highlight: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(NOTES_V1_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredNoteV1>(value.value()).and_then(StoredNoteV1::into_domain) {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.notes.push(entry);
                }
                Err(error) => eprintln!("failed to decode PDF note: {error:#}"),
            }
        }
    }

    load_shared_v1_storage(read_txn, library)?;
    {
        let table = read_txn.open_table(DOCUMENT_PREFERENCES_V1_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredDocumentPreferencesV1>(value.value())
                .and_then(StoredDocumentPreferencesV1::into_domain)
            {
                Ok(mut preferences) => {
                    preferences.path = normalize_path(&preferences.path);
                    library.document_preferences.push(preferences);
                }
                Err(error) => eprintln!("failed to decode document preferences: {error:#}"),
            }
        }
    }
    Ok(())
}

fn load_shared_v1_storage(read_txn: &redb::ReadTransaction, library: &mut Library) -> Result<()> {
    {
        let table = read_txn.open_table(AI_SESSIONS_V1_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredAiSessionV1>(value.value())
                .and_then(StoredAiSessionV1::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.ai_sessions.push(entry);
                }
                Err(error) => eprintln!("failed to decode AI session: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(UI_STATE_V1_TABLE)?;
        if let Some(value) = table.get(SIDEBAR_WIDTHS_KEY)? {
            match decode::<StoredSidebarWidthsV1>(value.value())
                .and_then(StoredSidebarWidthsV1::into_domain)
            {
                Ok(widths) => library.sidebar_widths = Some(widths),
                Err(error) => eprintln!("failed to decode sidebar widths: {error:#}"),
            }
        }
    }
    Ok(())
}

fn load_legacy_storage(read_txn: &redb::ReadTransaction, library: &mut Library) -> Result<()> {
    {
        let table = read_txn.open_table(HISTORY_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<LegacyHistoryEntry>(value.value()) {
                Ok(entry) => {
                    let mut entry = entry.into_domain();
                    entry.path = normalize_path(&entry.path);
                    library.history.push(entry);
                }
                Err(error) => eprintln!("failed to decode PDF history entry: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(BOOKMARKS_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<LegacyBookmark>(value.value()) {
                Ok(entry) => {
                    let mut entry = entry.into_domain();
                    entry.path = normalize_path(&entry.path);
                    library.bookmarks.push(entry);
                }
                Err(error) => eprintln!("failed to decode PDF bookmark: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(HIGHLIGHTS_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<LegacyHighlight>(value.value()) {
                Ok(entry) => {
                    let mut entry = entry.into_domain();
                    entry.path = normalize_path(&entry.path);
                    library.highlights.push(entry);
                }
                Err(error) => eprintln!("failed to decode PDF highlight: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(NOTES_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<LegacyNote>(value.value()) {
                Ok(entry) => {
                    let mut entry = entry.into_domain();
                    entry.path = normalize_path(&entry.path);
                    library.notes.push(entry);
                }
                Err(error) => eprintln!("failed to decode PDF note: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(AI_SESSIONS_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<AiSession>(value.value()) {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.ai_sessions.push(entry);
                }
                Err(error) => eprintln!("failed to decode AI session: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(UI_STATE_TABLE)?;
        if let Some(value) = table.get(SIDEBAR_WIDTHS_KEY)? {
            match decode::<SidebarWidths>(value.value()) {
                Ok(widths) => library.sidebar_widths = Some(widths),
                Err(error) => eprintln!("failed to decode sidebar widths: {error:#}"),
            }
        }
    }
    Ok(())
}

fn migrate_legacy_storage_v1(database: &Database, library: &Library) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        let migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        if migrations.get(STORAGE_V1_MIGRATION_KEY)?.is_some() {
            return Ok(());
        }
    }

    {
        let mut table = write_txn.open_table(HISTORY_V1_TABLE)?;
        for entry in &library.history {
            let key = history_key(&entry.path);
            let encoded = encode(&StoredHistoryEntryV1::from(entry))?;
            table.insert(key.as_slice(), encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(BOOKMARKS_V1_TABLE)?;
        for bookmark in &library.bookmarks {
            let encoded = encode(&StoredBookmarkV1::from(bookmark))?;
            table.insert(bookmark.id, encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(HIGHLIGHTS_V1_TABLE)?;
        for highlight in &library.highlights {
            let encoded = encode(&StoredHighlightV1::from(highlight))?;
            table.insert(highlight.id, encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(NOTES_V1_TABLE)?;
        for note in &library.notes {
            let encoded = encode(&StoredNoteV1::from(note))?;
            table.insert(note.id, encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(AI_SESSIONS_V1_TABLE)?;
        for session in &library.ai_sessions {
            let encoded = encode(&StoredAiSessionV1::from(session))?;
            table.insert(session.id, encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(UI_STATE_V1_TABLE)?;
        if let Some(widths) = library.sidebar_widths {
            let encoded = encode(&StoredSidebarWidthsV1::from(widths))?;
            table.insert(SIDEBAR_WIDTHS_KEY, encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V1_TABLE)?;
        for preferences in &library.document_preferences {
            let key = history_key(&preferences.path);
            let encoded = encode(&StoredDocumentPreferencesV1::from(preferences))?;
            table.insert(key.as_slice(), encoded.as_slice())?;
        }
    }
    {
        let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        migrations.insert(
            STORAGE_V1_MIGRATION_KEY,
            [STORAGE_SCHEMA_VERSION].as_slice(),
        )?;
    }
    write_txn.commit()?;
    Ok(())
}

fn migrate_storage_v2(database: &Database, library: &Library) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        let migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        if migrations.get(STORAGE_V2_MIGRATION_KEY)?.is_some() {
            return Ok(());
        }
    }
    {
        let mut table = write_txn.open_table(HISTORY_V2_TABLE)?;
        for entry in &library.history {
            let key = history_key(&entry.path);
            let encoded = encode(&StoredHistoryEntryV2::from(entry))?;
            table.insert(key.as_slice(), encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(BOOKMARKS_V2_TABLE)?;
        for bookmark in &library.bookmarks {
            let encoded = encode(&StoredBookmarkV2::from(bookmark))?;
            table.insert(bookmark.id, encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(HIGHLIGHTS_V2_TABLE)?;
        for highlight in &library.highlights {
            let encoded = encode(&StoredHighlightV2::from(highlight))?;
            table.insert(highlight.id, encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(NOTES_V2_TABLE)?;
        for note in &library.notes {
            let encoded = encode(&StoredNoteV2::from(note))?;
            table.insert(note.id, encoded.as_slice())?;
        }
    }
    {
        let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V2_TABLE)?;
        for preferences in &library.document_preferences {
            let key = history_key(&preferences.path);
            let encoded = encode(&StoredDocumentPreferencesV2::from(preferences))?;
            table.insert(key.as_slice(), encoded.as_slice())?;
        }
    }
    {
        let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        migrations.insert(STORAGE_V2_MIGRATION_KEY, [STORAGE_V2_VERSION].as_slice())?;
    }
    write_txn.commit()?;
    Ok(())
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    bincode::serialize(value).context("failed to encode library value")
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    bincode::deserialize(bytes).context("failed to decode library value")
}

fn require_storage_version(version: u8, expected: u8) -> Result<()> {
    if version == expected {
        Ok(())
    } else {
        Err(anyhow!(
            "unsupported library storage schema version: expected {expected}, found {version}"
        ))
    }
}

fn finite_fraction(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn valid_semantic_range(range: &SemanticTextRange) -> bool {
    !range.start.quote.is_empty()
        && !range.end.quote.is_empty()
        && range.start.logical_offset <= range.end.logical_offset
}

fn history_key(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        let mut key = Vec::with_capacity(path.as_os_str().as_bytes().len() + 1);
        key.push(0);
        key.extend_from_slice(path.as_os_str().as_bytes());
        key
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;

        let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
        let mut key = Vec::with_capacity(units.len() * 2 + 1);
        key.push(1);
        for unit in units {
            key.extend_from_slice(&unit.to_le_bytes());
        }
        key
    }
}

pub fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(page: usize) -> DocumentPosition {
        DocumentPosition::fixed(page as f32, page as f32 * 10.0, 1.25, page)
    }

    fn temp_library_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "glyphst-{name}-{}-{nanos}.redb",
            std::process::id()
        ))
    }

    #[test]
    fn history_is_deduplicated_and_sorted() {
        let mut library = Library::default();
        library.record_history_at(Path::new("/tmp/a.pdf"), position(1), 10);
        library.record_history_at(Path::new("/tmp/b.pdf"), position(2), 11);
        library.record_history_at(Path::new("/tmp/a.pdf"), position(3), 12);

        assert_eq!(library.history.len(), 2);
        assert_eq!(
            library.history[0].path,
            normalize_path(Path::new("/tmp/a.pdf"))
        );
        assert_eq!(library.history[0].position.page, 3);
        assert_eq!(
            library.history[1].path,
            normalize_path(Path::new("/tmp/b.pdf"))
        );
    }

    #[test]
    fn caps_history_entries() {
        let mut library = Library::default();
        for ix in 0..MAX_HISTORY_ENTRIES + 5 {
            library.record_history_at(
                Path::new(&format!("/tmp/{ix}.pdf")),
                position(ix + 1),
                ix as u64,
            );
        }

        assert_eq!(library.history.len(), MAX_HISTORY_ENTRIES);
        assert_eq!(library.history[0].opened, (MAX_HISTORY_ENTRIES + 4) as u64);
    }

    #[test]
    fn removes_missing_history_entries() {
        let database_path = temp_library_path("missing-history");
        let existing_path = temp_library_path("existing-history").with_extension("pdf");
        let missing_path = temp_library_path("missing-history-pdf").with_extension("pdf");
        fs::write(&existing_path, b"%PDF-1.4\n").unwrap();

        {
            let mut store = LibraryStore::open(database_path.clone()).unwrap();
            store.record_history(&existing_path, position(1));
            store.record_history(&missing_path, position(2));

            assert!(store.remove_missing_history_entries().unwrap());
            assert_eq!(store.history_entries().len(), 1);
            assert_eq!(
                store.history_entries()[0].path,
                normalize_path(&existing_path)
            );
            assert!(!store.remove_missing_history_entries().unwrap());
        }

        let _ = fs::remove_file(database_path);
        let _ = fs::remove_file(existing_path);
    }

    #[test]
    fn persists_history_bookmarks_and_highlights() {
        let path = temp_library_path("round-trip");
        let document_path = Path::new("/tmp/a.pdf");
        let highlight_rect = HighlightRect {
            page_idx: 0,
            x0: 10.0,
            y0: 20.0,
            x1: 30.0,
            y1: 40.0,
        };
        let highlight_id;
        let bookmark_id;
        let note_id;
        let ai_session_id;

        {
            let mut store = LibraryStore::open(path.clone()).unwrap();
            store.record_history(document_path, position(4));
            store.save().unwrap();
            bookmark_id = store
                .add_bookmark(document_path, "Chapter 1", position(5))
                .unwrap();
            let highlight = store
                .add_highlight(
                    document_path,
                    HighlightColor::Yellow,
                    vec![highlight_rect],
                    "selected text",
                    "",
                )
                .unwrap();
            highlight_id = highlight.id;
            assert!(
                store
                    .set_highlight_color(highlight_id, HighlightColor::Blue)
                    .unwrap()
            );
            let note = store
                .add_note(
                    document_path,
                    HighlightColor::Yellow,
                    vec![highlight_rect],
                    "selected text",
                    "initial note",
                )
                .unwrap();
            note_id = note.id;
            assert!(
                store
                    .update_note(note_id, "updated note", HighlightColor::Green)
                    .unwrap()
            );
            ai_session_id = store.next_ai_session_id();
            store
                .save_ai_session(AiSession {
                    id: ai_session_id,
                    path: document_path.to_path_buf(),
                    title: "Explain section".to_string(),
                    page: 4,
                    messages: vec![
                        AiSessionMessage {
                            role: AiSessionRole::User,
                            content: "Explain @selection".to_string(),
                            request_content: Some(
                                "Explain @selection\n\nAttached context".to_string(),
                            ),
                        },
                        AiSessionMessage {
                            role: AiSessionRole::Assistant,
                            content: "Explanation".to_string(),
                            request_content: None,
                        },
                    ],
                    created: 100,
                    updated: 101,
                })
                .unwrap();
        }

        {
            let mut store = LibraryStore::open(path.clone()).unwrap();
            assert_eq!(store.history_position(document_path).unwrap().page, 4);
            assert_eq!(store.bookmarks()[0].id, bookmark_id);
            let highlights = store.highlights_for(document_path);
            assert_eq!(highlights.len(), 1);
            assert_eq!(highlights[0].id, highlight_id);
            assert_eq!(highlights[0].color, HighlightColor::Blue);
            assert_eq!(highlights[0].rects, vec![highlight_rect]);
            let notes = store.notes_for(document_path);
            assert_eq!(notes.len(), 1);
            assert_eq!(notes[0].id, note_id);
            assert_eq!(notes[0].color, HighlightColor::Green);
            assert_eq!(notes[0].rects, vec![highlight_rect]);
            assert_eq!(notes[0].body, "updated note");
            let ai_sessions = store.ai_sessions_for(document_path);
            assert_eq!(ai_sessions.len(), 1);
            assert_eq!(ai_sessions[0].id, ai_session_id);
            assert_eq!(ai_sessions[0].title, "Explain section");
            assert_eq!(ai_sessions[0].messages.len(), 2);
            assert_eq!(
                ai_sessions[0].messages[0].request_content.as_deref(),
                Some("Explain @selection\n\nAttached context")
            );

            assert!(store.remove_highlight(highlight_id).unwrap());
            assert!(store.remove_note(note_id).unwrap());
            assert!(store.remove_bookmark(bookmark_id).unwrap());
        }

        {
            let store = LibraryStore::open(path.clone()).unwrap();
            assert!(store.bookmarks().is_empty());
            assert!(store.highlights_for(document_path).is_empty());
            assert!(store.notes_for(document_path).is_empty());
        }

        let _ = fs::remove_file(path);
    }

    #[test]
    fn persists_sidebar_widths() {
        let path = temp_library_path("sidebar-widths");
        let widths = SidebarWidths {
            thumbnails: 222.0,
            toc: 333.0,
            notes: 444.0,
            ai: 555.0,
        };

        {
            let mut store = LibraryStore::open(path.clone()).unwrap();
            store.set_sidebar_widths(widths).unwrap();
        }

        {
            let store = LibraryStore::open(path.clone()).unwrap();
            assert_eq!(store.sidebar_widths(), Some(widths));
        }

        let _ = fs::remove_file(path);
    }

    #[test]
    fn persists_document_target_language() {
        let database_path = temp_library_path("document-language");
        let document_path = Path::new("/tmp/translated.pdf");

        {
            let mut store = LibraryStore::open(database_path.clone()).unwrap();
            store
                .set_document_target_language(document_path, " Zh-Hans ")
                .unwrap();
            assert_eq!(
                store.document_target_language(document_path).as_deref(),
                Some("zh-hans")
            );
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            assert_eq!(
                store.document_target_language(document_path).as_deref(),
                Some("zh-hans")
            );
        }

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn history_save_fails_when_database_is_unavailable() {
        let document_path = Path::new("/tmp/unavailable-history.epub");
        let mut store = LibraryStore {
            library: Library::default(),
            database: None,
            startup_error: Some("database startup failed".to_string()),
        };
        store.record_history(document_path, position(1));

        let error = store.save().unwrap_err();

        assert_eq!(
            error.to_string(),
            "document library database is unavailable"
        );
        assert_eq!(store.history_position(document_path), Some(position(1)));
    }

    #[test]
    fn document_preference_writes_fail_when_database_is_unavailable() {
        let document_path = Path::new("/tmp/unavailable-preferences.epub");
        let preferences = EpubReaderPreferences {
            font_size: 19.0,
            line_height: 1.7,
            horizontal_margin: 40.0,
            max_content_width: 680.0,
            theme: glyphst_core::EpubTheme::Sepia,
            use_publisher_styles: false,
        };
        let mut store = LibraryStore {
            library: Library::default(),
            database: None,
            startup_error: Some("database startup failed".to_string()),
        };

        let language_error = store
            .set_document_target_language(document_path, "ja")
            .unwrap_err();
        assert_eq!(
            language_error.to_string(),
            "document library database is unavailable"
        );
        assert_eq!(store.document_target_language(document_path), None);

        let epub_error = store
            .set_epub_reader_preferences(document_path, preferences)
            .unwrap_err();
        assert_eq!(
            epub_error.to_string(),
            "document library database is unavailable"
        );
        assert_eq!(store.epub_reader_preferences(document_path), None);

        store
            .library
            .document_preferences
            .push(DocumentPreferences {
                path: normalize_path(document_path),
                target_language: "ja".to_string(),
                epub: Some(preferences),
            });
        let clear_error = store
            .clear_epub_reader_preferences(document_path)
            .unwrap_err();
        assert_eq!(
            clear_error.to_string(),
            "document library database is unavailable"
        );
        assert_eq!(
            store.document_target_language(document_path).as_deref(),
            Some("ja")
        );
        assert_eq!(
            store.epub_reader_preferences(document_path),
            Some(preferences)
        );
    }

    #[test]
    fn clears_epub_reader_preferences_and_preserves_target_language() {
        let database_path = temp_library_path("clear-epub-preferences");
        let document_path = Path::new("/tmp/book.epub");
        let preferences = EpubReaderPreferences {
            font_size: 19.0,
            line_height: 1.7,
            horizontal_margin: 40.0,
            max_content_width: 680.0,
            theme: glyphst_core::EpubTheme::Sepia,
            use_publisher_styles: false,
        };

        {
            let mut store = LibraryStore::open(database_path.clone()).unwrap();
            store
                .set_document_target_language(document_path, "ja")
                .unwrap();
            store
                .set_epub_reader_preferences(document_path, preferences)
                .unwrap();
            store.clear_epub_reader_preferences(document_path).unwrap();

            assert_eq!(store.epub_reader_preferences(document_path), None);
            assert_eq!(
                store.document_target_language(document_path).as_deref(),
                Some("ja")
            );
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            assert_eq!(store.epub_reader_preferences(document_path), None);
            assert_eq!(
                store.document_target_language(document_path).as_deref(),
                Some("ja")
            );
        }

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn epub_preferences_do_not_create_a_target_language_override() {
        let database_path = temp_library_path("epub-preferences-without-language");
        let document_path = Path::new("/tmp/book-with-global-language.epub");
        let preferences = EpubReaderPreferences {
            font_size: 19.0,
            line_height: 1.7,
            horizontal_margin: 40.0,
            max_content_width: 680.0,
            theme: glyphst_core::EpubTheme::Sepia,
            use_publisher_styles: false,
        };

        {
            let mut store = LibraryStore::open(database_path.clone()).unwrap();
            store
                .set_epub_reader_preferences(document_path, preferences)
                .unwrap();
            assert_eq!(store.document_target_language(document_path), None);
        }

        {
            let mut store = LibraryStore::open(database_path.clone()).unwrap();
            assert_eq!(store.document_target_language(document_path), None);
            assert_eq!(
                store.epub_reader_preferences(document_path),
                Some(preferences)
            );
            store.clear_epub_reader_preferences(document_path).unwrap();
            assert_eq!(store.library.document_preferences.len(), 0);
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            assert_eq!(store.document_target_language(document_path), None);
            assert_eq!(store.epub_reader_preferences(document_path), None);
        }

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn persists_epub_positions_annotations_and_preferences() {
        let database_path = temp_library_path("epub-round-trip");
        let document_path = PathBuf::from("/tmp/book.epub");
        let start = SemanticTextAnchor::new(12, "chapter text", "before ", " after");
        let end = SemanticTextAnchor::new(24, "selection end", "middle ", " suffix");
        let position = DocumentPosition::reflow(start.clone(), 0.25, 0.4, 1.5, 3);
        let range = SemanticTextRange {
            start: start.clone(),
            end: end.clone(),
        };
        let preferences = EpubReaderPreferences {
            font_size: 19.0,
            line_height: 1.7,
            horizontal_margin: 40.0,
            max_content_width: 680.0,
            theme: glyphst_core::EpubTheme::Sepia,
            use_publisher_styles: false,
        };

        {
            let mut store = LibraryStore::open(database_path.clone()).unwrap();
            store.record_history(&document_path, position.clone());
            store
                .add_bookmark(&document_path, "Chapter", position.clone())
                .unwrap();
            store
                .add_reflow_highlight(
                    &document_path,
                    HighlightColor::Yellow,
                    range.clone(),
                    Vec::new(),
                    "selected text",
                    "",
                )
                .unwrap();
            store
                .add_reflow_note(
                    &document_path,
                    HighlightColor::Blue,
                    range.clone(),
                    Vec::new(),
                    "selected text",
                    "remember this",
                )
                .unwrap();
            store
                .set_epub_reader_preferences(&document_path, preferences)
                .unwrap();
            store.save().unwrap();
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            assert_eq!(
                store.history_position(&document_path),
                Some(position.clone())
            );
            assert_eq!(store.bookmarks()[0].position, position);
            assert_eq!(
                store.highlights_for(&document_path)[0].semantic_range,
                Some(range.clone())
            );
            assert_eq!(
                store.notes_for(&document_path)[0].semantic_range,
                Some(range)
            );
            assert_eq!(
                store.epub_reader_preferences(&document_path),
                Some(preferences)
            );
        }

        let _ = fs::remove_file(database_path);
    }

    #[cfg(unix)]
    #[test]
    fn serializes_non_utf8_paths_without_loss() {
        use std::os::unix::ffi::OsStringExt as _;

        let path = PathBuf::from(std::ffi::OsString::from_vec(vec![
            b'/', b't', b'm', b'p', b'/', 0xff, b'.', b'p', b'd', b'f',
        ]));
        let entry = HistoryEntry {
            path,
            opened: 1,
            position: position(0),
        };

        assert_eq!(
            decode::<StoredHistoryEntryV1>(&encode(&StoredHistoryEntryV1::from(&entry)).unwrap())
                .unwrap()
                .into_domain()
                .unwrap(),
            entry
        );

        let preferences = DocumentPreferences {
            path: entry.path.clone(),
            target_language: "ja".to_string(),
            epub: None,
        };
        assert_eq!(
            decode::<StoredDocumentPreferencesV1>(
                &encode(&StoredDocumentPreferencesV1::from(&preferences)).unwrap()
            )
            .unwrap()
            .into_domain()
            .unwrap(),
            preferences
        );
    }

    #[cfg(unix)]
    #[test]
    fn persists_non_utf8_paths_for_all_path_records() {
        use std::os::unix::ffi::OsStringExt as _;

        let database_path = temp_library_path("non-utf8-paths");
        let document_path = PathBuf::from(std::ffi::OsString::from_vec(vec![
            b'/', b't', b'm', b'p', b'/', 0xff, b'.', b'p', b'd', b'f',
        ]));
        let rect = HighlightRect {
            page_idx: 1,
            x0: 1.0,
            y0: 2.0,
            x1: 3.0,
            y1: 4.0,
        };

        {
            let mut store = LibraryStore::open(database_path.clone()).unwrap();
            store.record_history(&document_path, position(1));
            store.save().unwrap();
            store
                .add_bookmark(&document_path, "bookmark", position(2))
                .unwrap();
            store
                .add_highlight(
                    &document_path,
                    HighlightColor::Yellow,
                    vec![rect],
                    "highlight",
                    "",
                )
                .unwrap();
            store
                .add_note(
                    &document_path,
                    HighlightColor::Blue,
                    vec![rect],
                    "note",
                    "body",
                )
                .unwrap();
            let session_id = store.next_ai_session_id();
            store
                .save_ai_session(AiSession {
                    id: session_id,
                    path: document_path.clone(),
                    title: "session".to_string(),
                    page: 1,
                    messages: vec![AiSessionMessage {
                        role: AiSessionRole::User,
                        content: "message".to_string(),
                        request_content: None,
                    }],
                    created: 1,
                    updated: 1,
                })
                .unwrap();
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            let normalized = normalize_path(&document_path);
            assert_eq!(store.history_entries()[0].path, normalized);
            assert_eq!(store.bookmarks()[0].path, normalized);
            assert_eq!(store.highlights_for(&document_path)[0].path, normalized);
            assert_eq!(store.notes_for(&document_path)[0].path, normalized);
            assert_eq!(store.ai_sessions_for(&document_path)[0].path, normalized);
        }

        let _ = fs::remove_file(database_path);
    }

    #[cfg(unix)]
    #[test]
    fn history_path_keys_do_not_collide_for_non_utf8_paths() {
        use std::os::unix::ffi::OsStringExt as _;

        let database_path = temp_library_path("history-key-collision");
        let raw_path = PathBuf::from(std::ffi::OsString::from_vec(vec![
            b'/', b't', b'm', b'p', b'/', 0xff, b'.', b'p', b'd', b'f',
        ]));
        let replacement_path = PathBuf::from("/tmp/\u{fffd}.pdf");
        assert_ne!(history_key(&raw_path), history_key(&replacement_path));

        {
            let mut store = LibraryStore::open(database_path.clone()).unwrap();
            store.record_history(&raw_path, position(1));
            store.record_history(&replacement_path, position(2));
            store.save().unwrap();
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            assert_eq!(store.history_entries().len(), 2);
            assert_eq!(store.history_position(&raw_path), Some(position(1)));
            assert_eq!(store.history_position(&replacement_path), Some(position(2)));
        }

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn migrates_legacy_storage_without_replacing_legacy_rows() {
        let database_path = temp_library_path("legacy-migration");
        let document_path = PathBuf::from("/tmp/legacy.pdf");
        let rect = HighlightRect {
            page_idx: 0,
            x0: 1.0,
            y0: 2.0,
            x1: 3.0,
            y1: 4.0,
        };
        let history = HistoryEntry {
            path: document_path.clone(),
            opened: 1,
            position: position(1),
        };
        let bookmark = Bookmark {
            id: 1,
            path: document_path.clone(),
            label: "legacy bookmark".to_string(),
            position: position(2),
            created: 2,
        };
        let highlight = Highlight {
            id: 1,
            path: document_path.clone(),
            color: HighlightColor::Yellow,
            rects: vec![rect],
            semantic_range: None,
            text: "legacy highlight".to_string(),
            note: String::new(),
            created: 3,
        };
        let note = Note {
            id: 1,
            path: document_path.clone(),
            color: HighlightColor::Blue,
            rects: vec![rect],
            semantic_range: None,
            text: "legacy note".to_string(),
            body: "legacy body".to_string(),
            created: 4,
        };
        let session = AiSession {
            id: 1,
            path: document_path.clone(),
            title: "legacy session".to_string(),
            page: 1,
            messages: vec![AiSessionMessage {
                role: AiSessionRole::User,
                content: "legacy message".to_string(),
                request_content: None,
            }],
            created: 5,
            updated: 6,
        };
        let widths = SidebarWidths {
            thumbnails: 1.0,
            toc: 2.0,
            notes: 3.0,
            ai: 4.0,
        };

        {
            let database = Database::create(&database_path).unwrap();
            ensure_tables(&database).unwrap();
            let write_txn = database.begin_write().unwrap();
            {
                let mut table = write_txn.open_table(HISTORY_TABLE).unwrap();
                table
                    .insert(
                        document_path.to_str().unwrap(),
                        encode(&LegacyHistoryEntry {
                            path: history.path.clone(),
                            opened: history.opened,
                            position: StoredDocumentPositionV1::from(&history.position),
                        })
                        .unwrap()
                        .as_slice(),
                    )
                    .unwrap();
            }
            {
                let mut table = write_txn.open_table(BOOKMARKS_TABLE).unwrap();
                table
                    .insert(
                        bookmark.id,
                        encode(&LegacyBookmark {
                            id: bookmark.id,
                            path: bookmark.path.clone(),
                            label: bookmark.label.clone(),
                            position: StoredDocumentPositionV1::from(&bookmark.position),
                            created: bookmark.created,
                        })
                        .unwrap()
                        .as_slice(),
                    )
                    .unwrap();
            }
            {
                let mut table = write_txn.open_table(HIGHLIGHTS_TABLE).unwrap();
                table
                    .insert(
                        highlight.id,
                        encode(&LegacyHighlight {
                            id: highlight.id,
                            path: highlight.path.clone(),
                            color: highlight.color,
                            rects: highlight.rects.clone(),
                            text: highlight.text.clone(),
                            note: highlight.note.clone(),
                            created: highlight.created,
                        })
                        .unwrap()
                        .as_slice(),
                    )
                    .unwrap();
            }
            {
                let mut table = write_txn.open_table(NOTES_TABLE).unwrap();
                table
                    .insert(
                        note.id,
                        encode(&LegacyNote {
                            id: note.id,
                            path: note.path.clone(),
                            color: note.color,
                            rects: note.rects.clone(),
                            text: note.text.clone(),
                            body: note.body.clone(),
                            created: note.created,
                        })
                        .unwrap()
                        .as_slice(),
                    )
                    .unwrap();
            }
            {
                let mut table = write_txn.open_table(AI_SESSIONS_TABLE).unwrap();
                table
                    .insert(session.id, encode(&session).unwrap().as_slice())
                    .unwrap();
            }
            {
                let mut table = write_txn.open_table(UI_STATE_TABLE).unwrap();
                table
                    .insert(SIDEBAR_WIDTHS_KEY, encode(&widths).unwrap().as_slice())
                    .unwrap();
            }
            write_txn.commit().unwrap();
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            assert_eq!(store.history_entries(), std::slice::from_ref(&history));
            assert_eq!(store.bookmarks(), std::slice::from_ref(&bookmark));
            assert_eq!(
                store.highlights_for(&document_path),
                vec![highlight.clone()]
            );
            assert_eq!(store.notes_for(&document_path), vec![note.clone()]);
            assert_eq!(store.ai_sessions_for(&document_path), vec![session.clone()]);
            assert_eq!(store.sidebar_widths(), Some(widths));
        }

        {
            let database = Database::open(&database_path).unwrap();
            let write_txn = database.begin_write().unwrap();
            {
                let mut table = write_txn.open_table(HISTORY_TABLE).unwrap();
                let replacement_history = LegacyHistoryEntry {
                    path: history.path.clone(),
                    opened: history.opened,
                    position: StoredDocumentPositionV1::from(&position(99)),
                };
                table
                    .insert(
                        document_path.to_str().unwrap(),
                        encode(&replacement_history).unwrap().as_slice(),
                    )
                    .unwrap();
            }
            write_txn.commit().unwrap();
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            assert_eq!(store.history_entries(), &[history]);
        }

        {
            let database = Database::open(&database_path).unwrap();
            let read_txn = database.begin_read().unwrap();
            let legacy_history = read_txn
                .open_table(HISTORY_TABLE)
                .unwrap()
                .get(document_path.to_str().unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(
                decode::<LegacyHistoryEntry>(legacy_history.value())
                    .unwrap()
                    .position,
                StoredDocumentPositionV1::from(&position(99))
            );
            assert!(
                read_txn
                    .open_table(STORAGE_MIGRATIONS_TABLE)
                    .unwrap()
                    .get(STORAGE_V1_MIGRATION_KEY)
                    .unwrap()
                    .is_some()
            );
        }

        let _ = fs::remove_file(database_path);
    }
}
