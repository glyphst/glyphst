use std::{
    collections::{HashMap, HashSet, VecDeque},
    error::Error,
    fmt, fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, anyhow};
pub use glyphst_core::{
    DocumentFormat, DocumentId, DocumentRotation, EpubReaderPreferences, HighlightColor,
    RegionImageReference, SemanticTextAnchor,
};
use gpui::Global;
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

const MAX_HISTORY_ENTRIES: usize = 100;
const MAX_DOCUMENT_EDIT_HISTORY_ENTRIES: usize = 100;
const MAX_DOCUMENT_EDIT_HISTORIES: usize = 32;
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
const DOCUMENT_PREFERENCES_V3_MIGRATION_KEY: &str = "document_preferences_v3";
const DOCUMENT_PREFERENCES_V3_VERSION: u8 = 3;
const DOCUMENT_PREFERENCES_V3_TABLE: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("document_preferences_v3");
const AI_SESSIONS_V2_MIGRATION_KEY: &str = "ai_sessions_v2";
const AI_SESSIONS_V2_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("ai_sessions_v2");
const AI_SESSIONS_V3_MIGRATION_KEY: &str = "ai_sessions_v3";
const AI_SESSIONS_V3_VERSION: u8 = 3;
const AI_SESSIONS_V3_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("ai_sessions_v3");
const AI_SESSIONS_V4_MIGRATION_KEY: &str = "ai_sessions_v4";
const AI_SESSIONS_V4_VERSION: u8 = 4;
const AI_SESSIONS_V4_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("ai_sessions_v4");
const SIDEBAR_WIDTHS_KEY: &str = "sidebar_widths";
const DOCUMENT_BROWSER_SIZE_KEY: &str = "document_browser_size";
const HOME_PREFERENCES_KEY: &str = "home_preferences";
const READER_SESSION_KEY: &str = "reader_session";
const READER_SESSION_STORAGE_V2: u8 = 2;
const READER_SESSION_STORAGE_V3: u8 = 3;
const READER_SESSION_STORAGE_V4: u8 = 4;
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
struct LegacyAiSession {
    id: u64,
    path: PathBuf,
    title: String,
    page: usize,
    messages: Vec<StoredAiSessionMessageV1>,
    created: u64,
    updated: u64,
}

impl LegacyAiSession {
    fn into_domain(self) -> AiSession {
        AiSession {
            id: self.id,
            path: self.path,
            title: self.title,
            page: self.page,
            messages: self
                .messages
                .into_iter()
                .map(AiSessionMessage::from)
                .collect(),
            created: self.created,
            updated: self.updated,
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
    messages: Vec<StoredAiSessionMessageV1>,
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
            messages: session
                .messages
                .iter()
                .map(StoredAiSessionMessageV1::from)
                .collect(),
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
            messages: self
                .messages
                .into_iter()
                .map(AiSessionMessage::from)
                .collect(),
            created: self.created,
            updated: self.updated,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredAiSessionMessageV1 {
    role: AiSessionRole,
    content: String,
    request_content: Option<String>,
}

impl From<&AiSessionMessage> for StoredAiSessionMessageV1 {
    fn from(message: &AiSessionMessage) -> Self {
        Self {
            role: message.role,
            content: message.content.clone(),
            request_content: message.request_content.clone(),
        }
    }
}

impl From<StoredAiSessionMessageV1> for AiSessionMessage {
    fn from(message: StoredAiSessionMessageV1) -> Self {
        Self {
            role: message.role,
            content: message.content,
            request_content: message.request_content,
            references: Vec::new(),
            occurrences: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredAiSessionMessageV2 {
    role: AiSessionRole,
    content: String,
    request_content: Option<String>,
    references: Vec<AiMentionReference>,
}

impl From<&AiSessionMessage> for StoredAiSessionMessageV2 {
    fn from(message: &AiSessionMessage) -> Self {
        Self {
            role: message.role,
            content: message.content.clone(),
            request_content: message.request_content.clone(),
            references: message.references.clone(),
        }
    }
}

impl From<StoredAiSessionMessageV2> for AiSessionMessage {
    fn from(message: StoredAiSessionMessageV2) -> Self {
        Self {
            role: message.role,
            content: message.content,
            request_content: message.request_content,
            references: message.references,
            occurrences: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredAiSessionV2 {
    version: u8,
    id: u64,
    path: StoredPathV1,
    title: String,
    page: usize,
    messages: Vec<StoredAiSessionMessageV2>,
    created: u64,
    updated: u64,
}

impl From<&AiSession> for StoredAiSessionV2 {
    fn from(session: &AiSession) -> Self {
        Self {
            version: STORAGE_V2_VERSION,
            id: session.id,
            path: StoredPathV1::from_path(&session.path),
            title: session.title.clone(),
            page: session.page,
            messages: session
                .messages
                .iter()
                .map(StoredAiSessionMessageV2::from)
                .collect(),
            created: session.created,
            updated: session.updated,
        }
    }
}

impl StoredAiSessionV2 {
    fn into_domain(self) -> Result<AiSession> {
        require_storage_version(self.version, STORAGE_V2_VERSION)?;
        Ok(AiSession {
            id: self.id,
            path: self.path.into_path()?,
            title: self.title,
            page: self.page,
            messages: self
                .messages
                .into_iter()
                .map(AiSessionMessage::from)
                .collect(),
            created: self.created,
            updated: self.updated,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredAiSessionMessageV3 {
    role: AiSessionRole,
    content: String,
    request_content: Option<String>,
    references: Vec<AiMentionReference>,
    occurrences: Vec<AiMentionOccurrence>,
}

impl From<&AiSessionMessage> for StoredAiSessionMessageV3 {
    fn from(message: &AiSessionMessage) -> Self {
        Self {
            role: message.role,
            content: message.content.clone(),
            request_content: message.request_content.clone(),
            references: message.references.clone(),
            occurrences: message.occurrences.clone(),
        }
    }
}

impl From<StoredAiSessionMessageV3> for AiSessionMessage {
    fn from(message: StoredAiSessionMessageV3) -> Self {
        Self {
            role: message.role,
            content: message.content,
            request_content: message.request_content,
            references: message.references,
            occurrences: message.occurrences,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredAiSessionV3 {
    version: u8,
    id: u64,
    path: StoredPathV1,
    title: String,
    page: usize,
    messages: Vec<StoredAiSessionMessageV3>,
    created: u64,
    updated: u64,
}

impl From<&AiSession> for StoredAiSessionV3 {
    fn from(session: &AiSession) -> Self {
        Self {
            version: AI_SESSIONS_V3_VERSION,
            id: session.id,
            path: StoredPathV1::from_path(&session.path),
            title: session.title.clone(),
            page: session.page,
            messages: session
                .messages
                .iter()
                .map(StoredAiSessionMessageV3::from)
                .collect(),
            created: session.created,
            updated: session.updated,
        }
    }
}

impl StoredAiSessionV3 {
    fn into_domain(self) -> Result<AiSession> {
        require_storage_version(self.version, AI_SESSIONS_V3_VERSION)?;
        Ok(AiSession {
            id: self.id,
            path: self.path.into_path()?,
            title: self.title,
            page: self.page,
            messages: self
                .messages
                .into_iter()
                .map(AiSessionMessage::from)
                .collect(),
            created: self.created,
            updated: self.updated,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredAiSessionV4 {
    version: u8,
    id: u64,
    path: StoredPathV1,
    title: String,
    page: usize,
    messages: Vec<StoredAiSessionMessageV3>,
    created: u64,
    updated: u64,
}

impl From<&AiSession> for StoredAiSessionV4 {
    fn from(session: &AiSession) -> Self {
        Self {
            version: AI_SESSIONS_V4_VERSION,
            id: session.id,
            path: StoredPathV1::from_path(&session.path),
            title: session.title.clone(),
            page: session.page,
            messages: session
                .messages
                .iter()
                .map(StoredAiSessionMessageV3::from)
                .collect(),
            created: session.created,
            updated: session.updated,
        }
    }
}

impl StoredAiSessionV4 {
    fn into_domain(self) -> Result<AiSession> {
        require_storage_version(self.version, AI_SESSIONS_V4_VERSION)?;
        Ok(AiSession {
            id: self.id,
            path: self.path.into_path()?,
            title: self.title,
            page: self.page,
            messages: self
                .messages
                .into_iter()
                .map(AiSessionMessage::from)
                .collect(),
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

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct StoredDocumentBrowserSizeV1 {
    version: u8,
    size: DocumentBrowserSize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredHomePreferencesV1 {
    version: u8,
    preferences: HomePreferences,
}

impl From<&HomePreferences> for StoredHomePreferencesV1 {
    fn from(preferences: &HomePreferences) -> Self {
        Self {
            version: STORAGE_SCHEMA_VERSION,
            preferences: preferences.clone(),
        }
    }
}

impl StoredHomePreferencesV1 {
    fn into_domain(self) -> Result<HomePreferences> {
        require_storage_version(self.version, STORAGE_SCHEMA_VERSION)?;
        Ok(self.preferences)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredReaderSessionV1 {
    version: u8,
    windows: Vec<StoredReaderWindowSessionV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredReaderSessionV2 {
    version: u8,
    windows: Vec<StoredReaderWindowSessionV2>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredReaderSessionV3 {
    version: u8,
    windows: Vec<StoredReaderWindowSessionV3>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredReaderSessionV4 {
    version: u8,
    windows: Vec<StoredReaderWindowSessionV4>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredReaderWindowSessionV4 {
    bounds: ReaderWindowBounds,
    pane_tree: StoredReaderPaneTreeV3,
    active_pane: u64,
    dock_document_tab: Option<u64>,
    left_sidebar: Option<ReaderSidebar>,
    ai_panel_open: bool,
    transparency_enabled: bool,
    synchronized_groups: Vec<ReaderSynchronizedGroup>,
    translation_sessions: Vec<StoredReaderTranslationSessionV2>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredReaderWindowSessionV3 {
    bounds: ReaderWindowBounds,
    pane_tree: StoredReaderPaneTreeV2,
    active_pane: u64,
    dock_document_tab: Option<u64>,
    left_sidebar: Option<ReaderSidebar>,
    ai_panel_open: bool,
    transparency_enabled: bool,
    synchronized_groups: Vec<ReaderSynchronizedGroup>,
    translation_sessions: Vec<StoredReaderTranslationSessionV2>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredReaderWindowSessionV2 {
    bounds: ReaderWindowBounds,
    pane_tree: StoredReaderPaneTreeV1,
    active_pane: u64,
    left_sidebar: Option<ReaderSidebar>,
    ai_panel_open: bool,
    transparency_enabled: bool,
    synchronized_groups: Vec<ReaderSynchronizedGroup>,
    translation_sessions: Vec<StoredReaderTranslationSessionV2>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredReaderTranslationSessionV2 {
    source_tab: u64,
    translated_tab: u64,
    source_language: Option<String>,
    target_language: String,
    mode: String,
    cache_key: Option<String>,
    original_unit_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredReaderWindowSessionV1 {
    bounds: ReaderWindowBounds,
    pane_tree: StoredReaderPaneTreeV1,
    active_pane: u64,
    left_sidebar: Option<ReaderSidebar>,
    ai_panel_open: bool,
    transparency_enabled: bool,
    synchronized_groups: Vec<ReaderSynchronizedGroup>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum StoredReaderPaneTreeV1 {
    Pane {
        id: u64,
        tabs: Vec<StoredReaderTabV1>,
        active_tab: Option<u64>,
    },
    Split {
        axis: ReaderSplitAxis,
        ratios: Vec<f32>,
        children: Vec<StoredReaderPaneTreeV1>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum StoredReaderPaneTreeV2 {
    Pane {
        id: u64,
        tabs: Vec<StoredReaderTabV2>,
        active_tab: Option<u64>,
    },
    Split {
        axis: ReaderSplitAxis,
        ratios: Vec<f32>,
        children: Vec<StoredReaderPaneTreeV2>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum StoredReaderPaneTreeV3 {
    Pane {
        id: u64,
        tabs: Vec<StoredReaderTabV3>,
        active_tab: Option<u64>,
    },
    Split {
        axis: ReaderSplitAxis,
        ratios: Vec<f32>,
        children: Vec<StoredReaderPaneTreeV3>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredReaderTabV3 {
    id: u64,
    kind: StoredReaderTabKindV3,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum StoredReaderTabKindV3 {
    NewTab(ReaderNewTabSession),
    Document {
        path: StoredPathV1,
        position: DocumentPosition,
    },
    Keymap,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredReaderTabV2 {
    id: u64,
    kind: StoredReaderTabKindV2,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum StoredReaderTabKindV2 {
    NewTab(StoredReaderNewTabSessionV3),
    Document {
        path: StoredPathV1,
        position: DocumentPosition,
    },
    Keymap,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredReaderNewTabSessionV3 {
    destination: ReaderNewTabDestination,
    view_mode: LibraryViewMode,
    sort: LibrarySort,
}

impl From<&ReaderNewTabSession> for StoredReaderNewTabSessionV3 {
    fn from(session: &ReaderNewTabSession) -> Self {
        let presentation = session.presentation(session.source);
        Self {
            destination: match session.source {
                QuickOpenSource::History => ReaderNewTabDestination::History,
                QuickOpenSource::Library => ReaderNewTabDestination::Library,
            },
            view_mode: presentation.view_mode,
            sort: presentation.sort,
        }
    }
}

impl StoredReaderNewTabSessionV3 {
    fn into_domain(self) -> ReaderNewTabSession {
        let source = match self.destination {
            ReaderNewTabDestination::Home | ReaderNewTabDestination::History => {
                QuickOpenSource::History
            }
            ReaderNewTabDestination::Library
            | ReaderNewTabDestination::Favorites
            | ReaderNewTabDestination::Tags
            | ReaderNewTabDestination::Trash
            | ReaderNewTabDestination::Tag(_)
            | ReaderNewTabDestination::SavedView(_) => QuickOpenSource::Library,
        };
        let mut session = ReaderNewTabSession::default();
        session.source = source;
        session.set_presentation(
            source,
            BrowserPresentation {
                view_mode: self.view_mode,
                sort: self.sort,
            },
        );
        session
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredReaderTabV1 {
    id: u64,
    kind: StoredReaderTabKindV1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum StoredReaderTabKindV1 {
    NewTab,
    Document {
        path: StoredPathV1,
        position: DocumentPosition,
    },
    Keymap,
}

impl From<&ReaderSessionSnapshot> for StoredReaderSessionV1 {
    fn from(session: &ReaderSessionSnapshot) -> Self {
        Self {
            version: STORAGE_SCHEMA_VERSION,
            windows: session
                .windows
                .iter()
                .map(StoredReaderWindowSessionV1::from)
                .collect(),
        }
    }
}

impl StoredReaderSessionV1 {
    fn into_domain(self) -> Result<ReaderSessionSnapshot> {
        require_storage_version(self.version, STORAGE_SCHEMA_VERSION)?;
        Ok(ReaderSessionSnapshot {
            windows: self
                .windows
                .into_iter()
                .map(StoredReaderWindowSessionV1::into_domain)
                .collect::<Result<_>>()?,
        })
    }
}

impl From<&ReaderSessionSnapshot> for StoredReaderSessionV2 {
    fn from(session: &ReaderSessionSnapshot) -> Self {
        Self {
            version: READER_SESSION_STORAGE_V2,
            windows: session
                .windows
                .iter()
                .map(StoredReaderWindowSessionV2::from)
                .collect(),
        }
    }
}

impl StoredReaderSessionV2 {
    fn into_domain(self) -> Result<ReaderSessionSnapshot> {
        require_storage_version(self.version, READER_SESSION_STORAGE_V2)?;
        Ok(ReaderSessionSnapshot {
            windows: self
                .windows
                .into_iter()
                .map(StoredReaderWindowSessionV2::into_domain)
                .collect::<Result<_>>()?,
        })
    }
}

impl From<&ReaderSessionSnapshot> for StoredReaderSessionV3 {
    fn from(session: &ReaderSessionSnapshot) -> Self {
        Self {
            version: READER_SESSION_STORAGE_V3,
            windows: session
                .windows
                .iter()
                .map(StoredReaderWindowSessionV3::from)
                .collect(),
        }
    }
}

impl StoredReaderSessionV3 {
    fn into_domain(self) -> Result<ReaderSessionSnapshot> {
        require_storage_version(self.version, READER_SESSION_STORAGE_V3)?;
        Ok(ReaderSessionSnapshot {
            windows: self
                .windows
                .into_iter()
                .map(StoredReaderWindowSessionV3::into_domain)
                .collect::<Result<_>>()?,
        })
    }
}

impl From<&ReaderSessionSnapshot> for StoredReaderSessionV4 {
    fn from(session: &ReaderSessionSnapshot) -> Self {
        Self {
            version: READER_SESSION_STORAGE_V4,
            windows: session
                .windows
                .iter()
                .map(StoredReaderWindowSessionV4::from)
                .collect(),
        }
    }
}

impl StoredReaderSessionV4 {
    fn into_domain(self) -> Result<ReaderSessionSnapshot> {
        require_storage_version(self.version, READER_SESSION_STORAGE_V4)?;
        Ok(ReaderSessionSnapshot {
            windows: self
                .windows
                .into_iter()
                .map(StoredReaderWindowSessionV4::into_domain)
                .collect::<Result<_>>()?,
        })
    }
}

impl From<&ReaderWindowSession> for StoredReaderWindowSessionV1 {
    fn from(window: &ReaderWindowSession) -> Self {
        Self {
            bounds: window.bounds,
            pane_tree: StoredReaderPaneTreeV1::from(&window.pane_tree),
            active_pane: window.active_pane,
            left_sidebar: window.left_sidebar,
            ai_panel_open: window.ai_panel_open,
            transparency_enabled: window.transparency_enabled,
            synchronized_groups: window.synchronized_groups.clone(),
        }
    }
}

impl StoredReaderWindowSessionV1 {
    fn into_domain(self) -> Result<ReaderWindowSession> {
        Ok(ReaderWindowSession {
            bounds: self.bounds.sanitized(),
            pane_tree: self.pane_tree.into_domain()?,
            active_pane: self.active_pane,
            dock_document_tab: None,
            left_sidebar: self.left_sidebar,
            ai_panel_open: self.ai_panel_open,
            transparency_enabled: self.transparency_enabled,
            synchronized_groups: self.synchronized_groups,
            translation_sessions: Vec::new(),
        })
    }
}

impl From<&ReaderWindowSession> for StoredReaderWindowSessionV2 {
    fn from(window: &ReaderWindowSession) -> Self {
        Self {
            bounds: window.bounds,
            pane_tree: StoredReaderPaneTreeV1::from(&window.pane_tree),
            active_pane: window.active_pane,
            left_sidebar: window.left_sidebar,
            ai_panel_open: window.ai_panel_open,
            transparency_enabled: window.transparency_enabled,
            synchronized_groups: window.synchronized_groups.clone(),
            translation_sessions: window
                .translation_sessions
                .iter()
                .map(StoredReaderTranslationSessionV2::from)
                .collect(),
        }
    }
}

impl StoredReaderWindowSessionV2 {
    fn into_domain(self) -> Result<ReaderWindowSession> {
        Ok(ReaderWindowSession {
            bounds: self.bounds.sanitized(),
            pane_tree: self.pane_tree.into_domain()?,
            active_pane: self.active_pane,
            dock_document_tab: None,
            left_sidebar: self.left_sidebar,
            ai_panel_open: self.ai_panel_open,
            transparency_enabled: self.transparency_enabled,
            synchronized_groups: self.synchronized_groups,
            translation_sessions: self
                .translation_sessions
                .into_iter()
                .map(StoredReaderTranslationSessionV2::into_domain)
                .collect(),
        })
    }
}

impl From<&ReaderWindowSession> for StoredReaderWindowSessionV3 {
    fn from(window: &ReaderWindowSession) -> Self {
        Self {
            bounds: window.bounds,
            pane_tree: StoredReaderPaneTreeV2::from(&window.pane_tree),
            active_pane: window.active_pane,
            dock_document_tab: window.dock_document_tab,
            left_sidebar: window.left_sidebar,
            ai_panel_open: window.ai_panel_open,
            transparency_enabled: window.transparency_enabled,
            synchronized_groups: window.synchronized_groups.clone(),
            translation_sessions: window
                .translation_sessions
                .iter()
                .map(StoredReaderTranslationSessionV2::from)
                .collect(),
        }
    }
}

impl StoredReaderWindowSessionV3 {
    fn into_domain(self) -> Result<ReaderWindowSession> {
        Ok(ReaderWindowSession {
            bounds: self.bounds.sanitized(),
            pane_tree: self.pane_tree.into_domain()?,
            active_pane: self.active_pane,
            dock_document_tab: self.dock_document_tab,
            left_sidebar: self.left_sidebar,
            ai_panel_open: self.ai_panel_open,
            transparency_enabled: self.transparency_enabled,
            synchronized_groups: self.synchronized_groups,
            translation_sessions: self
                .translation_sessions
                .into_iter()
                .map(StoredReaderTranslationSessionV2::into_domain)
                .collect(),
        })
    }
}

impl From<&ReaderWindowSession> for StoredReaderWindowSessionV4 {
    fn from(window: &ReaderWindowSession) -> Self {
        Self {
            bounds: window.bounds,
            pane_tree: StoredReaderPaneTreeV3::from(&window.pane_tree),
            active_pane: window.active_pane,
            dock_document_tab: window.dock_document_tab,
            left_sidebar: window.left_sidebar,
            ai_panel_open: window.ai_panel_open,
            transparency_enabled: window.transparency_enabled,
            synchronized_groups: window.synchronized_groups.clone(),
            translation_sessions: window
                .translation_sessions
                .iter()
                .map(StoredReaderTranslationSessionV2::from)
                .collect(),
        }
    }
}

impl StoredReaderWindowSessionV4 {
    fn into_domain(self) -> Result<ReaderWindowSession> {
        Ok(ReaderWindowSession {
            bounds: self.bounds.sanitized(),
            pane_tree: self.pane_tree.into_domain()?,
            active_pane: self.active_pane,
            dock_document_tab: self.dock_document_tab,
            left_sidebar: self.left_sidebar,
            ai_panel_open: self.ai_panel_open,
            transparency_enabled: self.transparency_enabled,
            synchronized_groups: self.synchronized_groups,
            translation_sessions: self
                .translation_sessions
                .into_iter()
                .map(StoredReaderTranslationSessionV2::into_domain)
                .collect(),
        })
    }
}

impl From<&ReaderTranslationSession> for StoredReaderTranslationSessionV2 {
    fn from(session: &ReaderTranslationSession) -> Self {
        Self {
            source_tab: session.source_tab,
            translated_tab: session.translated_tab,
            source_language: session.source_language.clone(),
            target_language: session.target_language.clone(),
            mode: session.mode.clone(),
            cache_key: session.cache_key.clone(),
            original_unit_ids: session.original_unit_ids.clone(),
        }
    }
}

impl StoredReaderTranslationSessionV2 {
    fn into_domain(self) -> ReaderTranslationSession {
        ReaderTranslationSession {
            source_tab: self.source_tab,
            translated_tab: self.translated_tab,
            source_language: self.source_language,
            target_language: self.target_language,
            mode: self.mode,
            cache_key: self.cache_key,
            original_unit_ids: self.original_unit_ids,
        }
    }
}

impl From<&ReaderPaneTree> for StoredReaderPaneTreeV1 {
    fn from(tree: &ReaderPaneTree) -> Self {
        match tree {
            ReaderPaneTree::Pane(pane) => Self::Pane {
                id: pane.id,
                tabs: pane.tabs.iter().map(StoredReaderTabV1::from).collect(),
                active_tab: pane.active_tab,
            },
            ReaderPaneTree::Split {
                axis,
                ratios,
                children,
            } => Self::Split {
                axis: *axis,
                ratios: ratios.clone(),
                children: children.iter().map(Self::from).collect(),
            },
        }
    }
}

impl StoredReaderPaneTreeV1 {
    fn into_domain(self) -> Result<ReaderPaneTree> {
        match self {
            Self::Pane {
                id,
                tabs,
                active_tab,
            } => Ok(ReaderPaneTree::Pane(ReaderPaneSession {
                id,
                tabs: tabs
                    .into_iter()
                    .map(StoredReaderTabV1::into_domain)
                    .collect::<Result<_>>()?,
                active_tab,
            })),
            Self::Split {
                axis,
                ratios,
                children,
            } => Ok(ReaderPaneTree::Split {
                axis,
                ratios: sanitize_ratios(ratios, children.len()),
                children: children
                    .into_iter()
                    .map(Self::into_domain)
                    .collect::<Result<_>>()?,
            }),
        }
    }
}

impl From<&ReaderPaneTree> for StoredReaderPaneTreeV2 {
    fn from(tree: &ReaderPaneTree) -> Self {
        match tree {
            ReaderPaneTree::Pane(pane) => Self::Pane {
                id: pane.id,
                tabs: pane.tabs.iter().map(StoredReaderTabV2::from).collect(),
                active_tab: pane.active_tab,
            },
            ReaderPaneTree::Split {
                axis,
                ratios,
                children,
            } => Self::Split {
                axis: *axis,
                ratios: ratios.clone(),
                children: children.iter().map(Self::from).collect(),
            },
        }
    }
}

impl StoredReaderPaneTreeV2 {
    fn into_domain(self) -> Result<ReaderPaneTree> {
        match self {
            Self::Pane {
                id,
                tabs,
                active_tab,
            } => Ok(ReaderPaneTree::Pane(ReaderPaneSession {
                id,
                tabs: tabs
                    .into_iter()
                    .map(StoredReaderTabV2::into_domain)
                    .collect::<Result<_>>()?,
                active_tab,
            })),
            Self::Split {
                axis,
                ratios,
                children,
            } => Ok(ReaderPaneTree::Split {
                axis,
                ratios: sanitize_ratios(ratios, children.len()),
                children: children
                    .into_iter()
                    .map(Self::into_domain)
                    .collect::<Result<_>>()?,
            }),
        }
    }
}

impl From<&ReaderPaneTree> for StoredReaderPaneTreeV3 {
    fn from(tree: &ReaderPaneTree) -> Self {
        match tree {
            ReaderPaneTree::Pane(pane) => Self::Pane {
                id: pane.id,
                tabs: pane.tabs.iter().map(StoredReaderTabV3::from).collect(),
                active_tab: pane.active_tab,
            },
            ReaderPaneTree::Split {
                axis,
                ratios,
                children,
            } => Self::Split {
                axis: *axis,
                ratios: ratios.clone(),
                children: children.iter().map(Self::from).collect(),
            },
        }
    }
}

impl StoredReaderPaneTreeV3 {
    fn into_domain(self) -> Result<ReaderPaneTree> {
        match self {
            Self::Pane {
                id,
                tabs,
                active_tab,
            } => Ok(ReaderPaneTree::Pane(ReaderPaneSession {
                id,
                tabs: tabs
                    .into_iter()
                    .map(StoredReaderTabV3::into_domain)
                    .collect::<Result<_>>()?,
                active_tab,
            })),
            Self::Split {
                axis,
                ratios,
                children,
            } => Ok(ReaderPaneTree::Split {
                axis,
                ratios: sanitize_ratios(ratios, children.len()),
                children: children
                    .into_iter()
                    .map(Self::into_domain)
                    .collect::<Result<_>>()?,
            }),
        }
    }
}

impl From<&ReaderTabSession> for StoredReaderTabV1 {
    fn from(tab: &ReaderTabSession) -> Self {
        let kind = match &tab.kind {
            ReaderTabKind::NewTab(_) => StoredReaderTabKindV1::NewTab,
            ReaderTabKind::Document { path, position } => StoredReaderTabKindV1::Document {
                path: StoredPathV1::from_path(path),
                position: position.clone(),
            },
            ReaderTabKind::Keymap => StoredReaderTabKindV1::Keymap,
        };
        Self { id: tab.id, kind }
    }
}

impl StoredReaderTabV1 {
    fn into_domain(self) -> Result<ReaderTabSession> {
        let kind = match self.kind {
            StoredReaderTabKindV1::NewTab => ReaderTabKind::NewTab(ReaderNewTabSession::default()),
            StoredReaderTabKindV1::Document { path, position } => ReaderTabKind::Document {
                path: normalize_path(&path.into_path()?),
                position,
            },
            StoredReaderTabKindV1::Keymap => ReaderTabKind::Keymap,
        };
        Ok(ReaderTabSession { id: self.id, kind })
    }
}

impl From<&ReaderTabSession> for StoredReaderTabV2 {
    fn from(tab: &ReaderTabSession) -> Self {
        let kind = match &tab.kind {
            ReaderTabKind::NewTab(state) => {
                StoredReaderTabKindV2::NewTab(StoredReaderNewTabSessionV3::from(state))
            }
            ReaderTabKind::Document { path, position } => StoredReaderTabKindV2::Document {
                path: StoredPathV1::from_path(path),
                position: position.clone(),
            },
            ReaderTabKind::Keymap => StoredReaderTabKindV2::Keymap,
        };
        Self { id: tab.id, kind }
    }
}

impl StoredReaderTabV2 {
    fn into_domain(self) -> Result<ReaderTabSession> {
        let kind = match self.kind {
            StoredReaderTabKindV2::NewTab(state) => ReaderTabKind::NewTab(state.into_domain()),
            StoredReaderTabKindV2::Document { path, position } => ReaderTabKind::Document {
                path: normalize_path(&path.into_path()?),
                position,
            },
            StoredReaderTabKindV2::Keymap => ReaderTabKind::Keymap,
        };
        Ok(ReaderTabSession { id: self.id, kind })
    }
}

impl From<&ReaderTabSession> for StoredReaderTabV3 {
    fn from(tab: &ReaderTabSession) -> Self {
        let kind = match &tab.kind {
            ReaderTabKind::NewTab(state) => StoredReaderTabKindV3::NewTab(state.clone()),
            ReaderTabKind::Document { path, position } => StoredReaderTabKindV3::Document {
                path: StoredPathV1::from_path(path),
                position: position.clone(),
            },
            ReaderTabKind::Keymap => StoredReaderTabKindV3::Keymap,
        };
        Self { id: tab.id, kind }
    }
}

impl StoredReaderTabV3 {
    fn into_domain(self) -> Result<ReaderTabSession> {
        let kind = match self.kind {
            StoredReaderTabKindV3::NewTab(state) => ReaderTabKind::NewTab(state),
            StoredReaderTabKindV3::Document { path, position } => ReaderTabKind::Document {
                path: normalize_path(&path.into_path()?),
                position,
            },
            StoredReaderTabKindV3::Keymap => ReaderTabKind::Keymap,
        };
        Ok(ReaderTabSession { id: self.id, kind })
    }
}

impl From<DocumentBrowserSize> for StoredDocumentBrowserSizeV1 {
    fn from(size: DocumentBrowserSize) -> Self {
        Self {
            version: STORAGE_SCHEMA_VERSION,
            size,
        }
    }
}

impl StoredDocumentBrowserSizeV1 {
    fn into_domain(self) -> Result<DocumentBrowserSize> {
        require_storage_version(self.version, STORAGE_SCHEMA_VERSION)?;
        Ok(self.size)
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
            rotation: DocumentRotation::default(),
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
            rotation: DocumentRotation::default(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredDocumentPreferencesV3 {
    version: u8,
    path: StoredPathV1,
    target_language: String,
    epub: Option<EpubReaderPreferences>,
    rotation: DocumentRotation,
}

impl From<&DocumentPreferences> for StoredDocumentPreferencesV3 {
    fn from(preferences: &DocumentPreferences) -> Self {
        Self {
            version: DOCUMENT_PREFERENCES_V3_VERSION,
            path: StoredPathV1::from_path(&preferences.path),
            target_language: preferences.target_language.clone(),
            epub: preferences.epub,
            rotation: preferences.rotation,
        }
    }
}

impl StoredDocumentPreferencesV3 {
    fn into_domain(self) -> Result<DocumentPreferences> {
        require_storage_version(self.version, DOCUMENT_PREFERENCES_V3_VERSION)?;
        Ok(DocumentPreferences {
            path: self.path.into_path()?,
            target_language: self.target_language,
            epub: self.epub,
            rotation: self.rotation,
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

impl Bookmark {
    fn normalize(&mut self) {
        self.label = self.label.trim().to_string();
    }

    fn is_valid(&self) -> bool {
        !self.path.as_os_str().is_empty() && !self.label.is_empty()
    }
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

impl Highlight {
    fn normalize(&mut self) {
        self.text = self.text.trim().to_string();
        self.note = self.note.trim().to_string();
    }

    fn is_valid(&self) -> bool {
        !self.path.as_os_str().is_empty()
            && (!self.rects.is_empty()
                || self
                    .semantic_range
                    .as_ref()
                    .is_some_and(valid_semantic_range))
    }
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

impl Note {
    fn normalize(&mut self) {
        self.text = self.text.trim().to_string();
        self.body = self.body.trim().to_string();
    }

    fn is_valid(&self) -> bool {
        !self.path.as_os_str().is_empty()
            && (!self.rects.is_empty()
                || self
                    .semantic_range
                    .as_ref()
                    .is_some_and(valid_semantic_range))
            && !self.body.is_empty()
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiSessionRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AiMentionReference {
    Selection,
    DocumentText,
    Page { page: usize },
    PageImage { page: usize },
    DocumentImage,
    RegionImage { reference: RegionImageReference },
    SelectedPagesText { pages: Vec<usize> },
    SelectedPagesImage { pages: Vec<usize> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiMentionOccurrence {
    pub start: usize,
    pub end: usize,
    pub reference: AiMentionReference,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiSessionMessage {
    pub role: AiSessionRole,
    pub content: String,
    pub request_content: Option<String>,
    pub references: Vec<AiMentionReference>,
    pub occurrences: Vec<AiMentionOccurrence>,
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

/// Preferred document-browser dimensions in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentBrowserSize {
    pub width: f32,
    pub height: f32,
}

/// Durable reader state used to restore windows when the next launch has no
/// explicit file argument. Transient overlays, searches, and dialogs are not
/// represented here.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReaderSessionSnapshot {
    pub windows: Vec<ReaderWindowSession>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReaderWindowSession {
    pub bounds: ReaderWindowBounds,
    pub pane_tree: ReaderPaneTree,
    pub active_pane: u64,
    pub dock_document_tab: Option<u64>,
    pub left_sidebar: Option<ReaderSidebar>,
    pub ai_panel_open: bool,
    pub transparency_enabled: bool,
    pub synchronized_groups: Vec<ReaderSynchronizedGroup>,
    pub translation_sessions: Vec<ReaderTranslationSession>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReaderWindowState {
    #[default]
    Windowed,
    Maximized,
    Fullscreen,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReaderWindowBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub state: ReaderWindowState,
}

impl ReaderWindowBounds {
    fn sanitized(self) -> Self {
        Self {
            x: finite_or(self.x, 0.0),
            y: finite_or(self.y, 0.0),
            width: finite_or(self.width, 1100.0).clamp(320.0, 16_384.0),
            height: finite_or(self.height, 800.0).clamp(240.0, 16_384.0),
            state: self.state,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReaderSplitAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReaderPaneTree {
    Pane(ReaderPaneSession),
    Split {
        axis: ReaderSplitAxis,
        ratios: Vec<f32>,
        children: Vec<ReaderPaneTree>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReaderPaneSession {
    pub id: u64,
    pub tabs: Vec<ReaderTabSession>,
    pub active_tab: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReaderTabSession {
    pub id: u64,
    pub kind: ReaderTabKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReaderNewTabDestination {
    #[default]
    Home,
    Library,
    Favorites,
    History,
    Tags,
    Trash,
    Tag(LibraryTagId),
    SavedView(LibrarySavedViewId),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuickOpenSource {
    #[default]
    History,
    Library,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserPresentation {
    pub view_mode: LibraryViewMode,
    pub sort: LibrarySort,
}

impl BrowserPresentation {
    pub fn history_default() -> Self {
        Self {
            view_mode: LibraryViewMode::Grouped,
            sort: LibrarySort {
                field: LibrarySortField::LastOpened,
                direction: LibrarySortDirection::Descending,
            },
        }
    }

    pub fn library_default() -> Self {
        Self {
            view_mode: LibraryViewMode::Table,
            sort: LibrarySort {
                field: LibrarySortField::DateAdded,
                direction: LibrarySortDirection::Descending,
            },
        }
    }

    pub fn trash_default() -> Self {
        Self::library_default()
    }
}

impl Default for BrowserPresentation {
    fn default() -> Self {
        Self::library_default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReaderNewTabSession {
    pub source: QuickOpenSource,
    pub history: BrowserPresentation,
    pub library: BrowserPresentation,
}

impl Default for ReaderNewTabSession {
    fn default() -> Self {
        Self {
            source: QuickOpenSource::History,
            history: BrowserPresentation::history_default(),
            library: BrowserPresentation::library_default(),
        }
    }
}

impl ReaderNewTabSession {
    pub fn presentation(&self, source: QuickOpenSource) -> BrowserPresentation {
        match source {
            QuickOpenSource::History => self.history,
            QuickOpenSource::Library => self.library,
        }
    }

    pub fn set_presentation(&mut self, source: QuickOpenSource, presentation: BrowserPresentation) {
        match source {
            QuickOpenSource::History => self.history = presentation,
            QuickOpenSource::Library => self.library = presentation,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HomeDestination {
    #[default]
    Library,
    History,
    Trash,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomePreferences {
    pub destination: HomeDestination,
    pub library: BrowserPresentation,
    pub history: BrowserPresentation,
    pub trash: BrowserPresentation,
}

impl Default for HomePreferences {
    fn default() -> Self {
        Self {
            destination: HomeDestination::Library,
            library: BrowserPresentation::library_default(),
            history: BrowserPresentation::history_default(),
            trash: BrowserPresentation::trash_default(),
        }
    }
}

impl HomePreferences {
    pub fn presentation(&self, destination: HomeDestination) -> BrowserPresentation {
        match destination {
            HomeDestination::Library => self.library,
            HomeDestination::History => self.history,
            HomeDestination::Trash => self.trash,
        }
    }

    pub fn set_presentation(
        &mut self,
        destination: HomeDestination,
        presentation: BrowserPresentation,
    ) {
        match destination {
            HomeDestination::Library => self.library = presentation,
            HomeDestination::History => self.history = presentation,
            HomeDestination::Trash => self.trash = presentation,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderTranslationSession {
    pub source_tab: u64,
    pub translated_tab: u64,
    pub source_language: Option<String>,
    pub target_language: String,
    pub mode: String,
    pub cache_key: Option<String>,
    pub original_unit_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReaderTabKind {
    NewTab(ReaderNewTabSession),
    Document {
        path: PathBuf,
        position: DocumentPosition,
    },
    Keymap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReaderSidebar {
    Thumbnails,
    TableOfContents,
    Notes,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReaderSynchronizedGroup {
    pub tabs: Vec<u64>,
    pub paused_tabs: Vec<u64>,
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

fn sanitize_ratios(ratios: Vec<f32>, child_count: usize) -> Vec<f32> {
    if child_count == 0 {
        return Vec::new();
    }
    if ratios.len() != child_count
        || ratios
            .iter()
            .any(|ratio| !ratio.is_finite() || *ratio <= 0.0)
    {
        return vec![1.0 / child_count as f32; child_count];
    }
    let total = ratios.iter().sum::<f32>();
    if !total.is_finite() || total <= f32::EPSILON {
        return vec![1.0 / child_count as f32; child_count];
    }
    ratios.into_iter().map(|ratio| ratio / total).collect()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentPreferences {
    pub path: PathBuf,
    pub target_language: String,
    pub epub: Option<EpubReaderPreferences>,
    pub rotation: DocumentRotation,
}

impl DocumentPreferences {
    fn is_empty(&self) -> bool {
        self.target_language.is_empty()
            && self.epub.is_none()
            && self.rotation == DocumentRotation::default()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Library {
    pub library_items: Vec<LibraryItem>,
    pub library_attachments: Vec<LibraryAttachment>,
    pub library_tags: Vec<LibraryTag>,
    pub library_item_tags: Vec<LibraryItemTag>,
    pub library_saved_views: Vec<LibrarySavedView>,
    pub library_collections: Vec<LibraryCollection>,
    pub library_item_locations: Vec<LibraryItemLocation>,
    pub history: Vec<HistoryEntry>,
    pub bookmarks: Vec<Bookmark>,
    pub highlights: Vec<Highlight>,
    pub notes: Vec<Note>,
    pub ai_sessions: Vec<AiSession>,
    pub sidebar_widths: Option<SidebarWidths>,
    pub document_browser_size: Option<DocumentBrowserSize>,
    pub home_preferences: HomePreferences,
    pub reader_session: Option<ReaderSessionSnapshot>,
    pub document_preferences: Vec<DocumentPreferences>,
    next_bookmark_id: u64,
    next_highlight_id: u64,
    next_note_id: u64,
    next_ai_session_id: u64,
    next_library_item_id: u64,
    next_library_attachment_id: u64,
    next_library_tag_id: u64,
    next_library_saved_view_id: u64,
    next_library_collection_id: u64,
}

pub struct LibraryStore {
    pub library: Library,
    database: Option<Arc<Database>>,
    managed_library_root: Option<PathBuf>,
    startup_error: Option<String>,
    document_edit_histories: HashMap<PathBuf, DocumentEditHistory>,
    next_document_edit_sequence: u64,
    bookmarks_revision: u64,
    history_entry_revisions: Arc<Mutex<HashMap<PathBuf, u64>>>,
    catalog_revision: u64,
}

pub struct HistoryEntryPersistenceSnapshot {
    database: Arc<Database>,
    entry: HistoryEntry,
    latest_revisions: Arc<Mutex<HashMap<PathBuf, u64>>>,
    revision: u64,
}

impl Global for LibraryStore {}

#[derive(Clone, Debug, PartialEq)]
enum DocumentEdit {
    Bookmark {
        before: Option<Bookmark>,
        after: Option<Bookmark>,
    },
    Highlight {
        before: Option<Highlight>,
        after: Option<Highlight>,
    },
    HighlightColor {
        before: Highlight,
        after: HighlightColor,
    },
    Note {
        before: Option<Note>,
        after: Option<Note>,
    },
    NoteUpdate {
        before: Note,
        after_body: String,
        after_color: HighlightColor,
    },
    Rotation {
        path: PathBuf,
        before: DocumentRotation,
        after: DocumentRotation,
    },
}

impl DocumentEdit {
    fn path(&self) -> &Path {
        match self {
            Self::Bookmark { before, after } => before
                .as_ref()
                .or(after.as_ref())
                .map(|bookmark| bookmark.path.as_path())
                .expect("bookmark edit must contain a snapshot"),
            Self::Highlight { before, after } => before
                .as_ref()
                .or(after.as_ref())
                .map(|highlight| highlight.path.as_path())
                .expect("highlight edit must contain a snapshot"),
            Self::HighlightColor { before, .. } => &before.path,
            Self::Note { before, after } => before
                .as_ref()
                .or(after.as_ref())
                .map(|note| note.path.as_path())
                .expect("note edit must contain a snapshot"),
            Self::NoteUpdate { before, .. } => &before.path,
            Self::Rotation { path, .. } => path,
        }
    }

    fn is_bookmark(&self) -> bool {
        matches!(self, Self::Bookmark { .. })
    }

    fn bump_revision(&self, revision: &mut DocumentEditRevision) {
        match self {
            Self::Bookmark { .. } => {}
            Self::Highlight { .. }
            | Self::HighlightColor { .. }
            | Self::Note { .. }
            | Self::NoteUpdate { .. } => {
                revision.annotations = revision.annotations.wrapping_add(1);
            }
            Self::Rotation { .. } => {
                revision.rotation = revision.rotation.wrapping_add(1);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocumentEditRevision {
    pub annotations: u64,
    pub rotation: u64,
}

#[derive(Default)]
struct DocumentEditHistory {
    undo: VecDeque<DocumentEdit>,
    redo: Vec<DocumentEdit>,
    revision: DocumentEditRevision,
    last_used: u64,
}

#[derive(Clone, Copy)]
enum DocumentEditDirection {
    Undo,
    Redo,
}

#[derive(Debug)]
struct DocumentEditConflict(&'static str);

impl fmt::Display for DocumentEditConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} changed outside undo history", self.0)
    }
}

impl Error for DocumentEditConflict {}

mod ai_sessions;
mod annotations;
mod bookmarks;
mod catalog;
mod edits;
mod history;
mod model;
mod preferences;
mod reader_session;
mod sidebar;
mod storage;
mod storage_helpers;

pub use catalog::*;
use storage_helpers::require_storage_version;

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

pub fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
