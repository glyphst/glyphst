use std::{
    collections::{HashMap, HashSet, VecDeque},
    error::Error,
    fmt, fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, anyhow};
pub use glyphst_core::{
    DocumentRotation, EpubReaderPreferences, HighlightColor, RegionImageReference,
    SemanticTextAnchor,
};
use gpui::Global;
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

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
    document_edit_histories: HashMap<PathBuf, DocumentEditHistory>,
    next_document_edit_sequence: u64,
    bookmarks_revision: u64,
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
                    document_edit_histories: HashMap::new(),
                    next_document_edit_sequence: 0,
                    bookmarks_revision: 0,
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
            document_edit_histories: HashMap::new(),
            next_document_edit_sequence: 0,
            bookmarks_revision: 0,
        };
        if let Err(error) = store.prune_ai_session_rows() {
            eprintln!("failed to prune expired AI sessions: {error:#}");
        }
        Ok(store)
    }

    pub fn startup_error(&self) -> Option<&str> {
        self.startup_error.as_deref()
    }

    pub fn document_edit_revision(&self, path: &Path) -> DocumentEditRevision {
        let path = normalize_path(path);
        self.document_edit_histories
            .get(&path)
            .map(|history| history.revision)
            .unwrap_or_default()
    }

    pub fn bookmarks_revision(&self) -> u64 {
        self.bookmarks_revision
    }

    pub fn can_undo_document(&self, path: &Path) -> bool {
        let path = normalize_path(path);
        self.document_edit_histories
            .get(&path)
            .is_some_and(|history| !history.undo.is_empty())
    }

    pub fn can_redo_document(&self, path: &Path) -> bool {
        let path = normalize_path(path);
        self.document_edit_histories
            .get(&path)
            .is_some_and(|history| !history.redo.is_empty())
    }

    pub fn undo_document(&mut self, path: &Path) -> Result<bool> {
        self.apply_history_edit(path, DocumentEditDirection::Undo)
    }

    pub fn redo_document(&mut self, path: &Path) -> Result<bool> {
        self.apply_history_edit(path, DocumentEditDirection::Redo)
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

    pub fn document_rotation(&self, path: &Path) -> DocumentRotation {
        let path = normalize_path(path);
        self.library
            .document_preferences
            .iter()
            .find(|preferences| preferences.path == path)
            .map(|preferences| preferences.rotation)
            .unwrap_or_default()
    }

    fn update_document_preferences(
        &mut self,
        path: &Path,
        update: impl FnOnce(&mut DocumentPreferences),
    ) -> Result<()> {
        let path = normalize_path(path);
        let existing_index = self
            .library
            .document_preferences
            .iter()
            .position(|preferences| preferences.path == path);
        let mut preferences = existing_index
            .map(|index| self.library.document_preferences[index].clone())
            .unwrap_or_else(|| DocumentPreferences {
                path: path.clone(),
                target_language: String::new(),
                epub: None,
                rotation: DocumentRotation::default(),
            });
        let previous = preferences.clone();
        update(&mut preferences);
        if preferences == previous {
            return Ok(());
        }
        if preferences.is_empty() {
            if let Some(index) = existing_index {
                self.delete_document_preferences(&path)?;
                self.library.document_preferences.remove(index);
            }
            return Ok(());
        }

        self.write_document_preferences(&preferences)?;
        if let Some(index) = existing_index {
            self.library.document_preferences[index] = preferences;
        } else {
            let index = self
                .library
                .document_preferences
                .binary_search_by(|existing| existing.path.cmp(&path))
                .unwrap_or_else(|index| index);
            self.library.document_preferences.insert(index, preferences);
        }
        Ok(())
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
        self.update_document_preferences(&path, |preferences| {
            preferences.target_language = target_language;
        })
    }

    pub fn set_epub_reader_preferences(
        &mut self,
        path: &Path,
        preferences: EpubReaderPreferences,
    ) -> Result<()> {
        let preferences = preferences.sanitized();
        self.update_document_preferences(path, |document_preferences| {
            document_preferences.epub = Some(preferences);
        })
    }

    pub fn set_document_rotation(
        &mut self,
        path: &Path,
        rotation: DocumentRotation,
    ) -> Result<bool> {
        let path = normalize_path(path);
        let before = self.document_rotation(&path);
        if before == rotation {
            return Ok(false);
        }
        self.update_document_preferences(&path, |preferences| {
            preferences.rotation = rotation;
        })?;
        self.record_document_edit(DocumentEdit::Rotation {
            path,
            before,
            after: rotation,
        });
        Ok(true)
    }

    pub fn clear_epub_reader_preferences(&mut self, path: &Path) -> Result<()> {
        self.update_document_preferences(path, |preferences| {
            preferences.epub = None;
        })
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
        let mut bookmark = Bookmark {
            id: self.library.next_bookmark_id.max(1),
            path: normalize_path(path),
            label: label.into(),
            position,
            created: current_timestamp(),
        };
        bookmark.normalize();
        self.replace_bookmark_snapshot(None, Some(&bookmark))?;
        self.record_document_edit(DocumentEdit::Bookmark {
            before: None,
            after: Some(bookmark.clone()),
        });
        Ok(bookmark.id)
    }

    pub fn remove_bookmark(&mut self, id: u64) -> Result<bool> {
        let Some(bookmark) = self
            .library
            .bookmarks
            .iter()
            .find(|bookmark| bookmark.id == id)
            .cloned()
        else {
            return Ok(false);
        };

        self.replace_bookmark_snapshot(Some(&bookmark), None)?;
        self.record_document_edit(DocumentEdit::Bookmark {
            before: Some(bookmark),
            after: None,
        });
        Ok(true)
    }

    pub fn add_highlight(
        &mut self,
        path: &Path,
        color: HighlightColor,
        rects: Vec<HighlightRect>,
        text: impl Into<String>,
        note: impl Into<String>,
    ) -> Result<Highlight> {
        self.add_highlight_with_geometry(
            path,
            color,
            AnnotationGeometry::fixed(rects),
            text.into(),
            note.into(),
        )
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
        self.add_highlight_with_geometry(
            path,
            color,
            AnnotationGeometry::reflow(rects, semantic_range),
            text.into(),
            note.into(),
        )
    }

    fn add_highlight_with_geometry(
        &mut self,
        path: &Path,
        color: HighlightColor,
        geometry: AnnotationGeometry,
        text: String,
        note: String,
    ) -> Result<Highlight> {
        let mut highlight = Highlight {
            id: self.library.next_highlight_id.max(1),
            path: normalize_path(path),
            color,
            rects: geometry.rects,
            semantic_range: geometry.semantic_range,
            text,
            note,
            created: current_timestamp(),
        };
        highlight.normalize();
        self.replace_highlight_snapshot(None, Some(&highlight))?;
        self.record_document_edit(DocumentEdit::Highlight {
            before: None,
            after: Some(highlight.clone()),
        });
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
        self.add_note_with_geometry(
            path,
            color,
            AnnotationGeometry::fixed(rects),
            text.into(),
            body.into(),
        )
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
        self.add_note_with_geometry(
            path,
            color,
            AnnotationGeometry::reflow(rects, semantic_range),
            text.into(),
            body.into(),
        )
    }

    fn add_note_with_geometry(
        &mut self,
        path: &Path,
        color: HighlightColor,
        geometry: AnnotationGeometry,
        text: String,
        body: String,
    ) -> Result<Note> {
        let mut note = Note {
            id: self.library.next_note_id.max(1),
            path: normalize_path(path),
            color,
            rects: geometry.rects,
            semantic_range: geometry.semantic_range,
            text,
            body,
            created: current_timestamp(),
        };
        note.normalize();
        self.replace_note_snapshot(None, Some(&note))?;
        self.record_document_edit(DocumentEdit::Note {
            before: None,
            after: Some(note.clone()),
        });
        Ok(note)
    }

    pub fn remove_highlight(&mut self, id: u64) -> Result<bool> {
        let Some(highlight) = self
            .library
            .highlights
            .iter()
            .find(|highlight| highlight.id == id)
            .cloned()
        else {
            return Ok(false);
        };

        self.replace_highlight_snapshot(Some(&highlight), None)?;
        self.record_document_edit(DocumentEdit::Highlight {
            before: Some(highlight),
            after: None,
        });
        Ok(true)
    }

    pub fn remove_note(&mut self, id: u64) -> Result<bool> {
        let Some(note) = self
            .library
            .notes
            .iter()
            .find(|note| note.id == id)
            .cloned()
        else {
            return Ok(false);
        };

        self.replace_note_snapshot(Some(&note), None)?;
        self.record_document_edit(DocumentEdit::Note {
            before: Some(note),
            after: None,
        });
        Ok(true)
    }

    pub fn set_highlight_color(&mut self, id: u64, color: HighlightColor) -> Result<bool> {
        let Some(before) = self
            .library
            .highlights
            .iter()
            .find(|highlight| highlight.id == id)
            .cloned()
        else {
            return Ok(false);
        };

        let mut after = before.clone();
        after.color = color;
        if after == before {
            return Ok(false);
        }
        self.replace_highlight_snapshot(Some(&before), Some(&after))?;
        self.record_document_edit(DocumentEdit::HighlightColor {
            before,
            after: color,
        });
        Ok(true)
    }

    pub fn update_note(
        &mut self,
        id: u64,
        body: impl Into<String>,
        color: HighlightColor,
    ) -> Result<bool> {
        let Some(before) = self
            .library
            .notes
            .iter()
            .find(|note| note.id == id)
            .cloned()
        else {
            return Ok(false);
        };

        let mut after = before.clone();
        after.body = body.into();
        after.color = color;
        after.normalize();
        if after == before {
            return Ok(false);
        }
        self.replace_note_snapshot(Some(&before), Some(&after))?;
        self.record_document_edit(DocumentEdit::NoteUpdate {
            before,
            after_body: after.body,
            after_color: after.color,
        });
        Ok(true)
    }

    fn record_document_edit(&mut self, edit: DocumentEdit) {
        let path = normalize_path(edit.path());
        let is_bookmark = edit.is_bookmark();
        let sequence = self.next_document_edit_sequence();
        let history = self.document_edit_histories.entry(path).or_default();
        if history.undo.len() == MAX_DOCUMENT_EDIT_HISTORY_ENTRIES {
            history.undo.pop_front();
        }
        edit.bump_revision(&mut history.revision);
        history.undo.push_back(edit);
        history.redo.clear();
        history.last_used = sequence;
        if is_bookmark {
            self.bookmarks_revision = self.bookmarks_revision.wrapping_add(1);
        }
        self.trim_document_edit_histories();
    }

    fn apply_history_edit(
        &mut self,
        path: &Path,
        direction: DocumentEditDirection,
    ) -> Result<bool> {
        let path = normalize_path(path);
        let Some(edit) = self
            .document_edit_histories
            .get(&path)
            .and_then(|history| match direction {
                DocumentEditDirection::Undo => history.undo.back(),
                DocumentEditDirection::Redo => history.redo.last(),
            })
            .cloned()
        else {
            return Ok(false);
        };
        if normalize_path(edit.path()) != path {
            return Err(anyhow!("document edit history path mismatch"));
        }

        if let Err(error) = self.apply_document_edit(&edit, direction) {
            if error.downcast_ref::<DocumentEditConflict>().is_some() {
                self.document_edit_histories.remove(&path);
            }
            return Err(error);
        }

        let is_bookmark = edit.is_bookmark();
        let sequence = self.next_document_edit_sequence();
        let history = self
            .document_edit_histories
            .get_mut(&path)
            .expect("document edit history must still exist");
        match direction {
            DocumentEditDirection::Undo => {
                let moved = history
                    .undo
                    .pop_back()
                    .expect("undo history must contain the applied edit");
                debug_assert_eq!(moved, edit);
                history.redo.push(moved);
            }
            DocumentEditDirection::Redo => {
                let moved = history
                    .redo
                    .pop()
                    .expect("redo history must contain the applied edit");
                debug_assert_eq!(moved, edit);
                if history.undo.len() == MAX_DOCUMENT_EDIT_HISTORY_ENTRIES {
                    history.undo.pop_front();
                }
                history.undo.push_back(moved);
            }
        }
        edit.bump_revision(&mut history.revision);
        history.last_used = sequence;
        if is_bookmark {
            self.bookmarks_revision = self.bookmarks_revision.wrapping_add(1);
        }
        Ok(true)
    }

    fn apply_document_edit(
        &mut self,
        edit: &DocumentEdit,
        direction: DocumentEditDirection,
    ) -> Result<()> {
        match edit {
            DocumentEdit::Bookmark { before, after } => {
                let (expected, target) = match direction {
                    DocumentEditDirection::Undo => (after.as_ref(), before.as_ref()),
                    DocumentEditDirection::Redo => (before.as_ref(), after.as_ref()),
                };
                self.replace_bookmark_snapshot(expected, target)
            }
            DocumentEdit::Highlight { before, after } => {
                let (expected, target) = match direction {
                    DocumentEditDirection::Undo => (after.as_ref(), before.as_ref()),
                    DocumentEditDirection::Redo => (before.as_ref(), after.as_ref()),
                };
                self.replace_highlight_snapshot(expected, target)
            }
            DocumentEdit::HighlightColor { before, after } => {
                let mut after_snapshot = before.clone();
                after_snapshot.color = *after;
                let (expected, target) = match direction {
                    DocumentEditDirection::Undo => (&after_snapshot, before),
                    DocumentEditDirection::Redo => (before, &after_snapshot),
                };
                self.replace_highlight_snapshot(Some(expected), Some(target))
            }
            DocumentEdit::Note { before, after } => {
                let (expected, target) = match direction {
                    DocumentEditDirection::Undo => (after.as_ref(), before.as_ref()),
                    DocumentEditDirection::Redo => (before.as_ref(), after.as_ref()),
                };
                self.replace_note_snapshot(expected, target)
            }
            DocumentEdit::NoteUpdate {
                before,
                after_body,
                after_color,
            } => {
                let mut after_snapshot = before.clone();
                after_snapshot.body.clone_from(after_body);
                after_snapshot.color = *after_color;
                let (expected, target) = match direction {
                    DocumentEditDirection::Undo => (&after_snapshot, before),
                    DocumentEditDirection::Redo => (before, &after_snapshot),
                };
                self.replace_note_snapshot(Some(expected), Some(target))
            }
            DocumentEdit::Rotation {
                path,
                before,
                after,
            } => {
                let (expected, target) = match direction {
                    DocumentEditDirection::Undo => (*after, *before),
                    DocumentEditDirection::Redo => (*before, *after),
                };
                if self.document_rotation(path) != expected {
                    return Err(DocumentEditConflict("document rotation").into());
                }
                self.update_document_preferences(path, |preferences| {
                    preferences.rotation = target;
                })
            }
        }
    }

    fn replace_bookmark_snapshot(
        &mut self,
        expected: Option<&Bookmark>,
        target: Option<&Bookmark>,
    ) -> Result<()> {
        let id = validate_snapshot_replacement(
            &self.library.bookmarks,
            expected,
            target,
            |bookmark| bookmark.id,
            |bookmark| bookmark.path.as_path(),
            validate_bookmark_snapshot,
            "bookmark",
        )?;

        if let Some(bookmark) = target {
            self.write_bookmark(bookmark)?;
        } else {
            self.remove_bookmark_from_database(id)?;
        }
        self.library.apply_bookmark_snapshot(id, target.cloned());
        Ok(())
    }

    fn replace_highlight_snapshot(
        &mut self,
        expected: Option<&Highlight>,
        target: Option<&Highlight>,
    ) -> Result<()> {
        let id = validate_snapshot_replacement(
            &self.library.highlights,
            expected,
            target,
            |highlight| highlight.id,
            |highlight| highlight.path.as_path(),
            validate_highlight_snapshot,
            "highlight",
        )?;

        if let Some(highlight) = target {
            self.write_highlight(highlight)?;
        } else {
            self.remove_highlight_from_database(id)?;
        }
        self.library.apply_highlight_snapshot(id, target.cloned());
        Ok(())
    }

    fn replace_note_snapshot(
        &mut self,
        expected: Option<&Note>,
        target: Option<&Note>,
    ) -> Result<()> {
        let id = validate_snapshot_replacement(
            &self.library.notes,
            expected,
            target,
            |note| note.id,
            |note| note.path.as_path(),
            validate_note_snapshot,
            "note",
        )?;

        if let Some(note) = target {
            self.write_note(note)?;
        } else {
            self.remove_note_from_database(id)?;
        }
        self.library.apply_note_snapshot(id, target.cloned());
        Ok(())
    }

    fn next_document_edit_sequence(&mut self) -> u64 {
        self.next_document_edit_sequence = self.next_document_edit_sequence.wrapping_add(1);
        self.next_document_edit_sequence
    }

    fn trim_document_edit_histories(&mut self) {
        while self.document_edit_histories.len() > MAX_DOCUMENT_EDIT_HISTORIES {
            let Some(path) = self
                .document_edit_histories
                .iter()
                .min_by_key(|(_, history)| history.last_used)
                .map(|(path, _)| path.clone())
            else {
                break;
            };
            self.document_edit_histories.remove(&path);
        }
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
            return Err(anyhow!("document library database is unavailable"));
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
            return Err(anyhow!("document library database is unavailable"));
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
            return Err(anyhow!("document library database is unavailable"));
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
            return Err(anyhow!("document library database is unavailable"));
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
            return Err(anyhow!("document library database is unavailable"));
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
            return Err(anyhow!("document library database is unavailable"));
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
            let mut table = write_txn.open_table(AI_SESSIONS_V3_TABLE)?;
            let encoded = encode(&StoredAiSessionV3::from(session))?;
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
            let mut table = write_txn.open_table(AI_SESSIONS_V3_TABLE)?;
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
        let encoded = encode(&StoredDocumentPreferencesV3::from(preferences))?;
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V3_TABLE)?;
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
            let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V3_TABLE)?;
            table.remove(key.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }
}

fn snapshot_id(expected: Option<u64>, target: Option<u64>) -> Result<u64> {
    match (expected, target) {
        (Some(expected), Some(target)) if expected != target => {
            Err(anyhow!("document edit snapshot id mismatch"))
        }
        (Some(id), _) | (_, Some(id)) => Ok(id),
        (None, None) => Err(anyhow!("document edit must contain a snapshot")),
    }
}

fn validate_snapshot_path(expected: Option<&Path>, target: Option<&Path>) -> Result<()> {
    if let (Some(expected), Some(target)) = (expected, target)
        && normalize_path(expected) != normalize_path(target)
    {
        return Err(anyhow!("document edit snapshot path mismatch"));
    }
    Ok(())
}

fn validate_bookmark_snapshot(bookmark: &Bookmark) -> Result<()> {
    if !bookmark.is_valid() {
        return Err(anyhow!("invalid bookmark snapshot"));
    }
    Ok(())
}

fn validate_highlight_snapshot(highlight: &Highlight) -> Result<()> {
    if !highlight.is_valid() {
        return Err(anyhow!("invalid highlight snapshot"));
    }
    Ok(())
}

fn validate_note_snapshot(note: &Note) -> Result<()> {
    if !note.is_valid() {
        return Err(anyhow!("invalid note snapshot"));
    }
    Ok(())
}

fn validate_snapshot_replacement<T: PartialEq>(
    snapshots: &[T],
    expected: Option<&T>,
    target: Option<&T>,
    id_of: impl Fn(&T) -> u64,
    path_of: impl for<'a> Fn(&'a T) -> &'a Path,
    validate: impl Fn(&T) -> Result<()>,
    kind: &'static str,
) -> Result<u64> {
    let id = snapshot_id(expected.map(&id_of), target.map(&id_of))?;
    validate_snapshot_path(expected.map(&path_of), target.map(&path_of))?;
    if let Some(target) = target {
        validate(target)?;
    }
    let current = snapshots.iter().find(|snapshot| id_of(snapshot) == id);
    if current != expected {
        return Err(DocumentEditConflict(kind).into());
    }
    Ok(id)
}

fn replace_snapshot_by_id<T>(
    snapshots: &mut Vec<T>,
    id: u64,
    target: Option<T>,
    snapshot_id: impl Fn(&T) -> u64,
) {
    snapshots.retain(|snapshot| snapshot_id(snapshot) != id);
    if let Some(target) = target {
        snapshots.push(target);
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

    fn apply_bookmark_snapshot(&mut self, id: u64, target: Option<Bookmark>) {
        replace_snapshot_by_id(&mut self.bookmarks, id, target, |bookmark| bookmark.id);
        self.bookmarks.sort_by(|left, right| {
            right
                .created
                .cmp(&left.created)
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.id.cmp(&right.id))
        });
        self.next_bookmark_id = self.next_bookmark_id.max(id.saturating_add(1));
    }

    fn apply_highlight_snapshot(&mut self, id: u64, target: Option<Highlight>) {
        replace_snapshot_by_id(&mut self.highlights, id, target, |highlight| highlight.id);
        self.highlights.sort_by(|left, right| {
            left.created
                .cmp(&right.created)
                .then_with(|| left.id.cmp(&right.id))
        });
        self.next_highlight_id = self.next_highlight_id.max(id.saturating_add(1));
    }

    fn apply_note_snapshot(&mut self, id: u64, target: Option<Note>) {
        replace_snapshot_by_id(&mut self.notes, id, target, |note| note.id);
        self.notes.sort_by(|left, right| {
            left.created
                .cmp(&right.created)
                .then_with(|| left.id.cmp(&right.id))
        });
        self.next_note_id = self.next_note_id.max(id.saturating_add(1));
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
            bookmark.normalize();
            bookmark.is_valid()
        });
        self.bookmarks.sort_by(|left, right| {
            right
                .created
                .cmp(&left.created)
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.id.cmp(&right.id))
        });

        self.highlights.retain_mut(|highlight| {
            highlight.normalize();
            highlight.is_valid()
        });
        self.highlights.sort_by(|left, right| {
            left.created
                .cmp(&right.created)
                .then_with(|| left.id.cmp(&right.id))
        });

        self.notes.retain_mut(|note| {
            note.normalize();
            note.is_valid()
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
                && (!preferences.target_language.is_empty()
                    || preferences.epub.is_some()
                    || preferences.rotation != DocumentRotation::default())
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
    {
        write_txn.open_table(DOCUMENT_PREFERENCES_V3_TABLE)?;
    }
    {
        write_txn.open_table(AI_SESSIONS_V2_TABLE)?;
    }
    {
        write_txn.open_table(AI_SESSIONS_V3_TABLE)?;
    }
    write_txn.commit()?;
    Ok(())
}

fn load_library(database: &Database) -> Result<Library> {
    let mut library = Library::default();
    let (
        has_v1_storage,
        has_v2_storage,
        has_document_preferences_v3,
        has_ai_sessions_v2,
        has_ai_sessions_v3,
    ) = {
        let read_txn = database.begin_read()?;
        let table = read_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        (
            table.get(STORAGE_V1_MIGRATION_KEY)?.is_some(),
            table.get(STORAGE_V2_MIGRATION_KEY)?.is_some(),
            table.get(DOCUMENT_PREFERENCES_V3_MIGRATION_KEY)?.is_some(),
            table.get(AI_SESSIONS_V2_MIGRATION_KEY)?.is_some(),
            table.get(AI_SESSIONS_V3_MIGRATION_KEY)?.is_some(),
        )
    };

    let read_txn = database.begin_read()?;
    if has_v2_storage {
        load_storage_v2(
            &read_txn,
            &mut library,
            has_ai_sessions_v2,
            has_ai_sessions_v3,
        )?;
    } else if has_v1_storage {
        load_storage_v1(
            &read_txn,
            &mut library,
            has_ai_sessions_v2,
            has_ai_sessions_v3,
        )?;
    } else {
        load_legacy_storage(
            &read_txn,
            &mut library,
            has_ai_sessions_v2,
            has_ai_sessions_v3,
        )?;
    }
    load_document_preferences(
        &read_txn,
        &mut library,
        has_document_preferences_v3,
        has_v2_storage,
        has_v1_storage,
    )?;
    drop(read_txn);

    library.sanitize();
    if !has_v1_storage {
        migrate_legacy_storage_v1(database, &library)?;
    }
    if !has_v2_storage {
        migrate_storage_v2(database, &library)?;
    }
    if !has_document_preferences_v3 {
        migrate_document_preferences_v3(database, &library)?;
    }
    if !has_ai_sessions_v2 {
        migrate_ai_sessions_v2(database, &library)?;
    }
    if !has_ai_sessions_v3 {
        migrate_ai_sessions_v3(database, &library)?;
    }
    Ok(library)
}

fn load_storage_v2(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
    has_ai_sessions_v2: bool,
    has_ai_sessions_v3: bool,
) -> Result<()> {
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
    load_ai_sessions(read_txn, library, has_ai_sessions_v2, has_ai_sessions_v3)?;
    load_shared_v1_storage(read_txn, library)?;
    Ok(())
}

fn load_storage_v1(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
    has_ai_sessions_v2: bool,
    has_ai_sessions_v3: bool,
) -> Result<()> {
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

    load_ai_sessions(read_txn, library, has_ai_sessions_v2, has_ai_sessions_v3)?;
    load_shared_v1_storage(read_txn, library)?;
    Ok(())
}

fn load_document_preferences(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
    has_v3: bool,
    has_v2: bool,
    has_v1: bool,
) -> Result<()> {
    if has_v3 {
        let table = read_txn.open_table(DOCUMENT_PREFERENCES_V3_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredDocumentPreferencesV3>(value.value())
                .and_then(StoredDocumentPreferencesV3::into_domain)
            {
                Ok(mut preferences) => {
                    preferences.path = normalize_path(&preferences.path);
                    library.document_preferences.push(preferences);
                }
                Err(error) => eprintln!("failed to decode document preferences: {error:#}"),
            }
        }
    } else if has_v2 {
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
    } else if has_v1 {
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
    let table = read_txn.open_table(UI_STATE_V1_TABLE)?;
    if let Some(value) = table.get(SIDEBAR_WIDTHS_KEY)? {
        match decode::<StoredSidebarWidthsV1>(value.value())
            .and_then(StoredSidebarWidthsV1::into_domain)
        {
            Ok(widths) => library.sidebar_widths = Some(widths),
            Err(error) => eprintln!("failed to decode sidebar widths: {error:#}"),
        }
    }
    Ok(())
}

fn load_ai_sessions(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
    has_ai_sessions_v2: bool,
    has_ai_sessions_v3: bool,
) -> Result<()> {
    if has_ai_sessions_v3 {
        let table = read_txn.open_table(AI_SESSIONS_V3_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredAiSessionV3>(value.value())
                .and_then(StoredAiSessionV3::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.ai_sessions.push(entry);
                }
                Err(error) => eprintln!("failed to decode AI session: {error:#}"),
            }
        }
    } else if has_ai_sessions_v2 {
        let table = read_txn.open_table(AI_SESSIONS_V2_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<StoredAiSessionV2>(value.value())
                .and_then(StoredAiSessionV2::into_domain)
            {
                Ok(mut entry) => {
                    entry.path = normalize_path(&entry.path);
                    library.ai_sessions.push(entry);
                }
                Err(error) => eprintln!("failed to decode AI session: {error:#}"),
            }
        }
    } else {
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
    Ok(())
}

fn load_legacy_storage(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
    has_ai_sessions_v2: bool,
    has_ai_sessions_v3: bool,
) -> Result<()> {
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

    if has_ai_sessions_v2 || has_ai_sessions_v3 {
        load_ai_sessions(read_txn, library, has_ai_sessions_v2, has_ai_sessions_v3)?;
    } else {
        let table = read_txn.open_table(AI_SESSIONS_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<LegacyAiSession>(value.value()) {
                Ok(entry) => {
                    let mut entry = entry.into_domain();
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

fn migrate_document_preferences_v3(database: &Database, library: &Library) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        let migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        if migrations
            .get(DOCUMENT_PREFERENCES_V3_MIGRATION_KEY)?
            .is_some()
        {
            return Ok(());
        }
    }
    {
        let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V3_TABLE)?;
        for preferences in &library.document_preferences {
            let key = history_key(&preferences.path);
            let encoded = encode(&StoredDocumentPreferencesV3::from(preferences))?;
            table.insert(key.as_slice(), encoded.as_slice())?;
        }
    }
    {
        let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        migrations.insert(
            DOCUMENT_PREFERENCES_V3_MIGRATION_KEY,
            [DOCUMENT_PREFERENCES_V3_VERSION].as_slice(),
        )?;
    }
    write_txn.commit()?;
    Ok(())
}

fn migrate_ai_sessions_v2(database: &Database, library: &Library) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        let migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        if migrations.get(AI_SESSIONS_V2_MIGRATION_KEY)?.is_some() {
            return Ok(());
        }
    }
    {
        let mut table = write_txn.open_table(AI_SESSIONS_V2_TABLE)?;
        for session in &library.ai_sessions {
            let encoded = encode(&StoredAiSessionV2::from(session))?;
            table.insert(session.id, encoded.as_slice())?;
        }
    }
    {
        let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        migrations.insert(
            AI_SESSIONS_V2_MIGRATION_KEY,
            [STORAGE_V2_VERSION].as_slice(),
        )?;
    }
    write_txn.commit()?;
    Ok(())
}

fn migrate_ai_sessions_v3(database: &Database, library: &Library) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        let migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        if migrations.get(AI_SESSIONS_V3_MIGRATION_KEY)?.is_some() {
            return Ok(());
        }
    }
    {
        let mut table = write_txn.open_table(AI_SESSIONS_V3_TABLE)?;
        for session in &library.ai_sessions {
            let encoded = encode(&StoredAiSessionV3::from(session))?;
            table.insert(session.id, encoded.as_slice())?;
        }
    }
    {
        let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE)?;
        migrations.insert(
            AI_SESSIONS_V3_MIGRATION_KEY,
            [AI_SESSIONS_V3_VERSION].as_slice(),
        )?;
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
                            references: Vec::new(),
                            occurrences: Vec::new(),
                        },
                        AiSessionMessage {
                            role: AiSessionRole::Assistant,
                            content: "Explanation".to_string(),
                            request_content: None,
                            references: Vec::new(),
                            occurrences: Vec::new(),
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
    fn undo_and_redo_cover_persistent_document_edits() {
        let database_path = temp_library_path("document-undo-redo");
        let document_path = Path::new("/tmp/document-undo-redo.pdf");
        let rect = HighlightRect {
            page_idx: 0,
            x0: 1.0,
            y0: 2.0,
            x1: 3.0,
            y1: 4.0,
        };
        let mut store = LibraryStore::open(database_path.clone()).unwrap();

        let bookmark_id = store
            .add_bookmark(document_path, "Chapter", position(1))
            .unwrap();
        let highlight = store
            .add_highlight(
                document_path,
                HighlightColor::Yellow,
                vec![rect],
                "highlight",
                "",
            )
            .unwrap();
        assert!(
            store
                .set_highlight_color(highlight.id, HighlightColor::Blue)
                .unwrap()
        );
        let note = store
            .add_note(
                document_path,
                HighlightColor::Yellow,
                vec![rect],
                "note",
                "before",
            )
            .unwrap();
        assert!(
            store
                .update_note(note.id, "after", HighlightColor::Green)
                .unwrap()
        );
        assert!(
            store
                .set_document_rotation(document_path, DocumentRotation::Degrees90)
                .unwrap()
        );

        assert!(store.undo_document(document_path).unwrap());
        assert_eq!(
            store.document_rotation(document_path),
            DocumentRotation::Degrees0
        );
        assert!(store.undo_document(document_path).unwrap());
        assert_eq!(store.notes_for(document_path)[0].body, "before");
        assert!(store.undo_document(document_path).unwrap());
        assert!(store.notes_for(document_path).is_empty());
        assert!(store.undo_document(document_path).unwrap());
        assert_eq!(
            store.highlights_for(document_path)[0].color,
            HighlightColor::Yellow
        );
        assert!(store.undo_document(document_path).unwrap());
        assert!(store.highlights_for(document_path).is_empty());
        assert!(store.undo_document(document_path).unwrap());
        assert!(store.bookmarks().is_empty());
        assert!(!store.can_undo_document(document_path));
        assert!(store.can_redo_document(document_path));

        for _ in 0..6 {
            assert!(store.redo_document(document_path).unwrap());
        }
        assert!(!store.can_redo_document(document_path));
        assert_eq!(store.bookmarks()[0].id, bookmark_id);
        assert_eq!(
            store.highlights_for(document_path)[0],
            Highlight {
                color: HighlightColor::Blue,
                ..highlight.clone()
            }
        );
        assert_eq!(
            store.notes_for(document_path)[0],
            Note {
                body: "after".to_string(),
                color: HighlightColor::Green,
                ..note
            }
        );
        assert_eq!(
            store.document_rotation(document_path),
            DocumentRotation::Degrees90
        );

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn undo_restores_deleted_objects_with_stable_ids() {
        let database_path = temp_library_path("document-delete-undo");
        let document_path = Path::new("/tmp/document-delete-undo.pdf");
        let rect = HighlightRect {
            page_idx: 0,
            x0: 1.0,
            y0: 2.0,
            x1: 3.0,
            y1: 4.0,
        };
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        let bookmark_id = store
            .add_bookmark(document_path, "Bookmark", position(1))
            .unwrap();
        let highlight = store
            .add_highlight(
                document_path,
                HighlightColor::Yellow,
                vec![rect],
                "highlight",
                "",
            )
            .unwrap();
        let note = store
            .add_note(
                document_path,
                HighlightColor::Blue,
                vec![rect],
                "note",
                "body",
            )
            .unwrap();

        assert!(store.remove_bookmark(bookmark_id).unwrap());
        assert!(store.undo_document(document_path).unwrap());
        assert_eq!(store.bookmarks()[0].id, bookmark_id);
        assert!(store.redo_document(document_path).unwrap());
        assert!(store.bookmarks().is_empty());

        assert!(store.remove_highlight(highlight.id).unwrap());
        assert!(store.undo_document(document_path).unwrap());
        assert_eq!(store.highlights_for(document_path)[0], highlight);
        assert!(store.redo_document(document_path).unwrap());
        assert!(store.highlights_for(document_path).is_empty());

        assert!(store.remove_note(note.id).unwrap());
        assert!(store.undo_document(document_path).unwrap());
        assert_eq!(store.notes_for(document_path)[0], note);
        assert!(store.redo_document(document_path).unwrap());
        assert!(store.notes_for(document_path).is_empty());

        let next_highlight = store
            .add_highlight(document_path, HighlightColor::Green, vec![rect], "next", "")
            .unwrap();
        assert!(next_highlight.id > highlight.id);
        assert!(!store.can_redo_document(document_path));

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn document_histories_are_independent_and_no_ops_preserve_redo() {
        let database_path = temp_library_path("independent-document-undo");
        let document_a = Path::new("/tmp/undo-a.pdf");
        let document_b = Path::new("/tmp/undo-b.pdf");
        let rect = HighlightRect {
            page_idx: 0,
            x0: 1.0,
            y0: 2.0,
            x1: 3.0,
            y1: 4.0,
        };
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        let highlight_a = store
            .add_highlight(document_a, HighlightColor::Yellow, vec![rect], "a", "")
            .unwrap();
        store
            .add_highlight(document_b, HighlightColor::Blue, vec![rect], "b", "")
            .unwrap();
        assert!(
            store
                .set_highlight_color(highlight_a.id, HighlightColor::Green)
                .unwrap()
        );
        assert!(store.undo_document(document_a).unwrap());
        assert_eq!(
            store.highlights_for(document_a)[0].color,
            HighlightColor::Yellow
        );
        assert_eq!(store.highlights_for(document_b).len(), 1);
        assert!(
            !store
                .set_highlight_color(highlight_a.id, HighlightColor::Yellow)
                .unwrap()
        );
        assert!(store.can_redo_document(document_a));
        assert!(store.redo_document(document_a).unwrap());
        assert_eq!(
            store.highlights_for(document_a)[0].color,
            HighlightColor::Green
        );
        assert!(store.undo_document(document_a).unwrap());
        store
            .add_note(
                document_a,
                HighlightColor::Yellow,
                vec![rect],
                "note",
                "body",
            )
            .unwrap();
        assert!(!store.can_redo_document(document_a));
        assert!(store.can_undo_document(document_b));

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn revisions_distinguish_document_content_from_global_bookmarks() {
        let database_path = temp_library_path("document-edit-revisions");
        let document_a = Path::new("/tmp/revision-a.pdf");
        let document_b = Path::new("/tmp/revision-b.pdf");
        let rect = HighlightRect {
            page_idx: 0,
            x0: 1.0,
            y0: 2.0,
            x1: 3.0,
            y1: 4.0,
        };
        let mut store = LibraryStore::open(database_path.clone()).unwrap();

        let bookmark_id = store
            .add_bookmark(document_b, "Bookmark", position(1))
            .unwrap();
        assert_eq!(
            store.document_edit_revision(document_a),
            DocumentEditRevision::default()
        );
        assert_eq!(
            store.document_edit_revision(document_b),
            DocumentEditRevision::default()
        );
        assert_eq!(store.bookmarks_revision(), 1);
        assert!(store.remove_bookmark(bookmark_id).unwrap());
        assert_eq!(store.bookmarks_revision(), 2);
        assert!(store.undo_document(document_b).unwrap());
        assert_eq!(store.bookmarks_revision(), 3);

        store
            .add_highlight(
                document_a,
                HighlightColor::Yellow,
                vec![rect],
                "highlight",
                "",
            )
            .unwrap();
        assert_eq!(
            store.document_edit_revision(document_a),
            DocumentEditRevision {
                annotations: 1,
                rotation: 0,
            }
        );
        assert_eq!(
            store.document_edit_revision(document_b),
            DocumentEditRevision::default()
        );
        assert_eq!(store.bookmarks_revision(), 3);
        assert!(
            store
                .set_document_rotation(document_a, DocumentRotation::Degrees90)
                .unwrap()
        );
        assert_eq!(
            store.document_edit_revision(document_a),
            DocumentEditRevision {
                annotations: 1,
                rotation: 1,
            }
        );

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn per_document_history_drops_oldest_edits() {
        let database_path = temp_library_path("document-edit-capacity");
        let document_path = Path::new("/tmp/document-edit-capacity.pdf");
        let mut store = LibraryStore::open(database_path.clone()).unwrap();

        for index in 0..MAX_DOCUMENT_EDIT_HISTORY_ENTRIES + 1 {
            store
                .add_bookmark(document_path, format!("Bookmark {index}"), position(index))
                .unwrap();
        }
        for _ in 0..MAX_DOCUMENT_EDIT_HISTORY_ENTRIES {
            assert!(store.undo_document(document_path).unwrap());
        }
        assert!(!store.undo_document(document_path).unwrap());
        assert_eq!(store.bookmarks().len(), 1);

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn rotation_undo_preserves_other_document_preferences() {
        let database_path = temp_library_path("rotation-undo-preferences");
        let document_path = Path::new("/tmp/rotation-undo-preferences.epub");
        let preferences = EpubReaderPreferences {
            font_size: 19.0,
            line_height: 1.6,
            horizontal_margin: 32.0,
            max_content_width: 720.0,
            theme: glyphst_core::EpubTheme::Sepia,
            use_publisher_styles: false,
        };
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        store
            .set_document_target_language(document_path, "ja")
            .unwrap();
        store
            .set_epub_reader_preferences(document_path, preferences)
            .unwrap();
        assert!(
            store
                .set_document_rotation(document_path, DocumentRotation::Degrees180)
                .unwrap()
        );

        assert!(store.undo_document(document_path).unwrap());
        assert_eq!(
            store.document_rotation(document_path),
            DocumentRotation::Degrees0
        );
        assert_eq!(
            store.document_target_language(document_path).as_deref(),
            Some("ja")
        );
        assert_eq!(
            store.epub_reader_preferences(document_path),
            Some(preferences)
        );

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn failed_edits_do_not_move_and_conflicts_clear_history() {
        let document_path = Path::new("/tmp/failed-document-undo.pdf");
        let rect = HighlightRect {
            page_idx: 0,
            x0: 1.0,
            y0: 2.0,
            x1: 3.0,
            y1: 4.0,
        };
        let mut unavailable = LibraryStore {
            library: Library::default(),
            database: None,
            startup_error: Some("database startup failed".to_string()),
            document_edit_histories: HashMap::new(),
            next_document_edit_sequence: 0,
            bookmarks_revision: 0,
        };
        assert!(
            unavailable
                .add_highlight(
                    document_path,
                    HighlightColor::Yellow,
                    vec![rect],
                    "highlight",
                    "",
                )
                .is_err()
        );
        assert!(unavailable.highlights_for(document_path).is_empty());
        assert!(!unavailable.can_undo_document(document_path));

        let database_path = temp_library_path("conflicting-document-undo");
        let mut store = LibraryStore::open(database_path.clone()).unwrap();
        let note = store
            .add_note(
                document_path,
                HighlightColor::Yellow,
                vec![rect],
                "note",
                "body",
            )
            .unwrap();
        assert!(
            store
                .update_note(note.id, "   ", HighlightColor::Blue)
                .is_err()
        );
        assert_eq!(store.notes_for(document_path)[0], note);
        assert!(store.undo_document(document_path).unwrap());
        assert!(store.notes_for(document_path).is_empty());

        let highlight = store
            .add_highlight(
                document_path,
                HighlightColor::Yellow,
                vec![rect],
                "highlight",
                "",
            )
            .unwrap();
        store
            .library
            .highlights
            .iter_mut()
            .find(|stored| stored.id == highlight.id)
            .unwrap()
            .color = HighlightColor::Blue;
        assert!(store.undo_document(document_path).is_err());
        assert!(!store.can_undo_document(document_path));
        assert!(!store.can_redo_document(document_path));

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn edit_history_is_bounded_and_not_persisted() {
        let database_path = temp_library_path("bounded-document-undo");
        let oldest_path = PathBuf::from("/tmp/bounded-document-0.pdf");
        {
            let mut store = LibraryStore::open(database_path.clone()).unwrap();
            for index in 0..=MAX_DOCUMENT_EDIT_HISTORIES {
                let path = PathBuf::from(format!("/tmp/bounded-document-{index}.pdf"));
                store
                    .add_bookmark(&path, format!("Bookmark {index}"), position(index))
                    .unwrap();
            }
            assert!(!store.can_undo_document(&oldest_path));
            assert!(store.can_undo_document(Path::new(&format!(
                "/tmp/bounded-document-{}.pdf",
                MAX_DOCUMENT_EDIT_HISTORIES
            ))));
        }

        let store = LibraryStore::open(database_path.clone()).unwrap();
        assert!(!store.can_undo_document(Path::new("/tmp/bounded-document-32.pdf")));
        assert_eq!(store.bookmarks().len(), MAX_DOCUMENT_EDIT_HISTORIES + 1);

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn loads_v1_ai_sessions_and_migrates_them_without_references() {
        let database_path = temp_library_path("ai-session-v1-compatibility");
        let document_path = PathBuf::from("/tmp/v1-session.pdf");
        let stored_session = StoredAiSessionV1 {
            version: STORAGE_SCHEMA_VERSION,
            id: 7,
            path: StoredPathV1::from_path(&document_path),
            title: "V1 session".to_string(),
            page: 3,
            messages: vec![StoredAiSessionMessageV1 {
                role: AiSessionRole::User,
                content: "Explain @current page text".to_string(),
                request_content: Some("Explain the attached page text".to_string()),
            }],
            created: 10,
            updated: 11,
        };

        {
            let database = Database::create(&database_path).unwrap();
            ensure_tables(&database).unwrap();
            let write_txn = database.begin_write().unwrap();
            {
                let mut table = write_txn.open_table(AI_SESSIONS_V1_TABLE).unwrap();
                table
                    .insert(
                        stored_session.id,
                        encode(&stored_session).unwrap().as_slice(),
                    )
                    .unwrap();
            }
            {
                let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE).unwrap();
                migrations
                    .insert(
                        STORAGE_V1_MIGRATION_KEY,
                        [STORAGE_SCHEMA_VERSION].as_slice(),
                    )
                    .unwrap();
                migrations
                    .insert(STORAGE_V2_MIGRATION_KEY, [STORAGE_V2_VERSION].as_slice())
                    .unwrap();
            }
            write_txn.commit().unwrap();
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            let sessions = store.ai_sessions_for(&document_path);
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].id, stored_session.id);
            assert_eq!(sessions[0].messages.len(), 1);
            assert!(sessions[0].messages[0].references.is_empty());
            assert!(sessions[0].messages[0].occurrences.is_empty());
        }

        {
            let database = Database::open(&database_path).unwrap();
            let read_txn = database.begin_read().unwrap();
            assert!(
                read_txn
                    .open_table(STORAGE_MIGRATIONS_TABLE)
                    .unwrap()
                    .get(AI_SESSIONS_V2_MIGRATION_KEY)
                    .unwrap()
                    .is_some()
            );
            assert!(
                read_txn
                    .open_table(STORAGE_MIGRATIONS_TABLE)
                    .unwrap()
                    .get(AI_SESSIONS_V3_MIGRATION_KEY)
                    .unwrap()
                    .is_some()
            );
            let v1_value = read_txn
                .open_table(AI_SESSIONS_V1_TABLE)
                .unwrap()
                .get(stored_session.id)
                .unwrap()
                .unwrap();
            assert_eq!(
                decode::<StoredAiSessionV1>(v1_value.value())
                    .unwrap()
                    .messages
                    .len(),
                1
            );
            let v2_value = read_txn
                .open_table(AI_SESSIONS_V2_TABLE)
                .unwrap()
                .get(stored_session.id)
                .unwrap()
                .unwrap();
            assert!(
                decode::<StoredAiSessionV2>(v2_value.value())
                    .unwrap()
                    .messages[0]
                    .references
                    .is_empty()
            );
            let v3_value = read_txn
                .open_table(AI_SESSIONS_V3_TABLE)
                .unwrap()
                .get(stored_session.id)
                .unwrap()
                .unwrap();
            assert!(
                decode::<StoredAiSessionV3>(v3_value.value())
                    .unwrap()
                    .messages[0]
                    .occurrences
                    .is_empty()
            );
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            assert_eq!(store.ai_sessions_for(&document_path).len(), 1);
        }

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn loads_genuinely_v2_encoded_ai_session_after_domain_occurrences_are_added() {
        #[derive(Serialize)]
        struct HistoricalAiSessionMessageV2 {
            role: AiSessionRole,
            content: String,
            request_content: Option<String>,
            references: Vec<AiMentionReference>,
        }

        #[derive(Serialize)]
        struct HistoricalAiSessionV2 {
            version: u8,
            id: u64,
            path: StoredPathV1,
            title: String,
            page: usize,
            messages: Vec<HistoricalAiSessionMessageV2>,
            created: u64,
            updated: u64,
        }

        let database_path = temp_library_path("ai-session-v2-compatibility");
        let document_path = PathBuf::from("/tmp/genuine-v2-session.pdf");
        let stored_session = HistoricalAiSessionV2 {
            version: STORAGE_V2_VERSION,
            id: 8,
            path: StoredPathV1::from_path(&document_path),
            title: "V2 session".to_string(),
            page: 4,
            messages: vec![HistoricalAiSessionMessageV2 {
                role: AiSessionRole::User,
                content: "Explain @selection twice".to_string(),
                request_content: Some("Explain the attached selections".to_string()),
                references: vec![AiMentionReference::Selection, AiMentionReference::Selection],
            }],
            created: 12,
            updated: 13,
        };

        {
            let database = Database::create(&database_path).unwrap();
            ensure_tables(&database).unwrap();
            let write_txn = database.begin_write().unwrap();
            {
                let mut table = write_txn.open_table(AI_SESSIONS_V2_TABLE).unwrap();
                table
                    .insert(
                        stored_session.id,
                        encode(&stored_session).unwrap().as_slice(),
                    )
                    .unwrap();
            }
            {
                let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE).unwrap();
                migrations
                    .insert(
                        STORAGE_V1_MIGRATION_KEY,
                        [STORAGE_SCHEMA_VERSION].as_slice(),
                    )
                    .unwrap();
                migrations
                    .insert(STORAGE_V2_MIGRATION_KEY, [STORAGE_V2_VERSION].as_slice())
                    .unwrap();
                migrations
                    .insert(
                        AI_SESSIONS_V2_MIGRATION_KEY,
                        [STORAGE_V2_VERSION].as_slice(),
                    )
                    .unwrap();
            }
            write_txn.commit().unwrap();
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            let sessions = store.ai_sessions_for(&document_path);
            assert_eq!(sessions.len(), 1);
            assert_eq!(
                sessions[0].messages[0].references,
                vec![AiMentionReference::Selection, AiMentionReference::Selection]
            );
            assert!(sessions[0].messages[0].occurrences.is_empty());
        }

        {
            let database = Database::open(&database_path).unwrap();
            let read_txn = database.begin_read().unwrap();
            assert!(
                read_txn
                    .open_table(STORAGE_MIGRATIONS_TABLE)
                    .unwrap()
                    .get(AI_SESSIONS_V3_MIGRATION_KEY)
                    .unwrap()
                    .is_some()
            );
            let v3_value = read_txn
                .open_table(AI_SESSIONS_V3_TABLE)
                .unwrap()
                .get(stored_session.id)
                .unwrap()
                .unwrap();
            let migrated = decode::<StoredAiSessionV3>(v3_value.value()).unwrap();
            assert_eq!(migrated.messages[0].references.len(), 2);
            assert!(migrated.messages[0].occurrences.is_empty());
        }

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn round_trips_v3_ai_session_mention_references_without_image_bytes() {
        let database_path = temp_library_path("ai-session-v3-references");
        let document_path = PathBuf::from("/tmp/v3-session.pdf");
        let references = vec![
            AiMentionReference::Selection,
            AiMentionReference::DocumentText,
            AiMentionReference::Page { page: 4 },
            AiMentionReference::PageImage { page: 5 },
            AiMentionReference::DocumentImage,
            AiMentionReference::RegionImage {
                reference: RegionImageReference::new([glyphst_core::PageRegion {
                    page: glyphst_core::PageIndex(5),
                    x0: 10.0,
                    y0: 20.0,
                    x1: 30.0,
                    y1: 40.0,
                }]),
            },
        ];

        {
            let mut store = LibraryStore::open(database_path.clone()).unwrap();
            let session_id = store.next_ai_session_id();
            store
                .save_ai_session(AiSession {
                    id: session_id,
                    path: document_path.clone(),
                    title: "Referenced context".to_string(),
                    page: 5,
                    messages: vec![AiSessionMessage {
                        role: AiSessionRole::User,
                        content: "Compare the references".to_string(),
                        request_content: Some("Compare the attached context".to_string()),
                        references: references.clone(),
                        occurrences: Vec::new(),
                    }],
                    created: 20,
                    updated: 21,
                })
                .unwrap();
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            let sessions = store.ai_sessions_for(&document_path);
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].messages[0].references, references);
        }

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn round_trips_v3_occurrences_with_duplicate_references() {
        let database_path = temp_library_path("ai-session-v3-duplicate-occurrences");
        let document_path = PathBuf::from("/tmp/v3-duplicate-occurrences.pdf");
        let reference = AiMentionReference::Selection;
        let occurrences = vec![
            AiMentionOccurrence {
                start: 0,
                end: 10,
                reference: reference.clone(),
            },
            AiMentionOccurrence {
                start: 15,
                end: 25,
                reference: reference.clone(),
            },
        ];

        {
            let mut store = LibraryStore::open(database_path.clone()).unwrap();
            let session_id = store.next_ai_session_id();
            store
                .save_ai_session(AiSession {
                    id: session_id,
                    path: document_path.clone(),
                    title: "Duplicate mentions".to_string(),
                    page: 1,
                    messages: vec![AiSessionMessage {
                        role: AiSessionRole::User,
                        content: "@selection and @selection".to_string(),
                        request_content: Some("Compare the attached selections".to_string()),
                        references: vec![reference.clone(), reference],
                        occurrences: occurrences.clone(),
                    }],
                    created: 30,
                    updated: 31,
                })
                .unwrap();
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            let sessions = store.ai_sessions_for(&document_path);
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].messages[0].occurrences, occurrences);
            assert_eq!(sessions[0].messages[0].references.len(), 2);
        }

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn round_trips_v3_region_occurrence_geometry() {
        let database_path = temp_library_path("ai-session-v3-region-geometry");
        let document_path = PathBuf::from("/tmp/v3-region-geometry.pdf");
        let region_reference = RegionImageReference::new([
            glyphst_core::PageRegion {
                page: glyphst_core::PageIndex(2),
                x0: 10.25,
                y0: 20.5,
                x1: 30.75,
                y1: 40.125,
            },
            glyphst_core::PageRegion {
                page: glyphst_core::PageIndex(3),
                x0: 1.5,
                y0: 2.25,
                x1: 99.5,
                y1: 120.875,
            },
        ]);
        let occurrence = AiMentionOccurrence {
            start: 9,
            end: 22,
            reference: AiMentionReference::RegionImage {
                reference: region_reference.clone(),
            },
        };

        {
            let mut store = LibraryStore::open(database_path.clone()).unwrap();
            let session_id = store.next_ai_session_id();
            store
                .save_ai_session(AiSession {
                    id: session_id,
                    path: document_path.clone(),
                    title: "Region geometry".to_string(),
                    page: 3,
                    messages: vec![AiSessionMessage {
                        role: AiSessionRole::User,
                        content: "Describe @region-image".to_string(),
                        request_content: Some("Describe the attached image region".to_string()),
                        references: vec![AiMentionReference::RegionImage {
                            reference: region_reference.clone(),
                        }],
                        occurrences: vec![occurrence.clone()],
                    }],
                    created: 40,
                    updated: 41,
                })
                .unwrap();
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            let sessions = store.ai_sessions_for(&document_path);
            assert_eq!(
                sessions[0].messages[0].occurrences,
                vec![occurrence.clone()]
            );
            let AiMentionReference::RegionImage { reference } =
                &sessions[0].messages[0].occurrences[0].reference
            else {
                panic!("expected a region image occurrence");
            };
            assert_eq!(reference, &region_reference);
        }

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn ai_mention_reference_keeps_historical_variant_encoding_compatible() {
        #[derive(Serialize)]
        enum HistoricalAiMentionReference {
            Selection,
            DocumentText,
            Page { page: usize },
            PageImage { page: usize },
        }

        let historical = [
            (
                HistoricalAiMentionReference::Selection,
                AiMentionReference::Selection,
            ),
            (
                HistoricalAiMentionReference::DocumentText,
                AiMentionReference::DocumentText,
            ),
            (
                HistoricalAiMentionReference::Page { page: 7 },
                AiMentionReference::Page { page: 7 },
            ),
            (
                HistoricalAiMentionReference::PageImage { page: 9 },
                AiMentionReference::PageImage { page: 9 },
            ),
        ];

        for (historical, expected) in historical {
            let bytes = encode(&historical).unwrap();
            assert_eq!(decode::<AiMentionReference>(&bytes).unwrap(), expected);
        }
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
            document_edit_histories: HashMap::new(),
            next_document_edit_sequence: 0,
            bookmarks_revision: 0,
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
            document_edit_histories: HashMap::new(),
            next_document_edit_sequence: 0,
            bookmarks_revision: 0,
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

        let rotation_error = store
            .set_document_rotation(document_path, DocumentRotation::Degrees90)
            .unwrap_err();
        assert_eq!(
            rotation_error.to_string(),
            "document library database is unavailable"
        );
        assert_eq!(
            store.document_rotation(document_path),
            DocumentRotation::default()
        );

        store
            .library
            .document_preferences
            .push(DocumentPreferences {
                path: normalize_path(document_path),
                target_language: "ja".to_string(),
                epub: Some(preferences),
                rotation: DocumentRotation::default(),
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
    fn preference_setters_preserve_other_document_fields() {
        let database_path = temp_library_path("preference-field-preservation");
        let document_path = Path::new("/tmp/preference-fields.epub");
        let rotation_only_path = Path::new("/tmp/rotation-only.epub");
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
                .set_document_rotation(document_path, DocumentRotation::Degrees90)
                .unwrap();
            store
                .set_epub_reader_preferences(document_path, preferences)
                .unwrap();
            assert_eq!(
                store.document_rotation(document_path),
                DocumentRotation::Degrees90
            );

            store
                .set_document_target_language(document_path, "JA")
                .unwrap();
            assert_eq!(
                store.epub_reader_preferences(document_path),
                Some(preferences)
            );
            assert_eq!(
                store.document_rotation(document_path),
                DocumentRotation::Degrees90
            );

            store.clear_epub_reader_preferences(document_path).unwrap();
            assert_eq!(
                store.document_target_language(document_path).as_deref(),
                Some("ja")
            );
            assert_eq!(
                store.document_rotation(document_path),
                DocumentRotation::Degrees90
            );

            store
                .set_document_rotation(rotation_only_path, DocumentRotation::Degrees180)
                .unwrap();
            store
                .set_epub_reader_preferences(rotation_only_path, preferences)
                .unwrap();
            store
                .clear_epub_reader_preferences(rotation_only_path)
                .unwrap();
            assert_eq!(
                store.document_rotation(rotation_only_path),
                DocumentRotation::Degrees180
            );
            assert_eq!(store.epub_reader_preferences(rotation_only_path), None);
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            assert_eq!(
                store.document_target_language(document_path).as_deref(),
                Some("ja")
            );
            assert_eq!(
                store.document_rotation(document_path),
                DocumentRotation::Degrees90
            );
            assert_eq!(
                store.document_rotation(rotation_only_path),
                DocumentRotation::Degrees180
            );
        }

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn persists_document_rotation_round_trip() {
        let database_path = temp_library_path("document-rotation-round-trip");
        let document_path = Path::new("/tmp/rotated.pdf");

        {
            let mut store = LibraryStore::open(database_path.clone()).unwrap();
            store
                .set_document_rotation(document_path, DocumentRotation::Degrees270)
                .unwrap();
            assert_eq!(
                store.document_rotation(document_path),
                DocumentRotation::Degrees270
            );
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            assert_eq!(
                store.document_rotation(document_path),
                DocumentRotation::Degrees270
            );
        }

        let _ = fs::remove_file(database_path);
    }

    #[test]
    fn migrates_v2_document_preferences_with_default_rotation() {
        let database_path = temp_library_path("document-preferences-v2-migration");
        let document_path = PathBuf::from("/tmp/v2-preferences.epub");
        let epub = EpubReaderPreferences {
            font_size: 19.0,
            line_height: 1.7,
            horizontal_margin: 40.0,
            max_content_width: 680.0,
            theme: glyphst_core::EpubTheme::Sepia,
            use_publisher_styles: false,
        };
        let stored_preferences = StoredDocumentPreferencesV2 {
            version: STORAGE_V2_VERSION,
            path: StoredPathV1::from_path(&document_path),
            target_language: "ja".to_string(),
            epub: Some(epub),
        };

        {
            let database = Database::create(&database_path).unwrap();
            ensure_tables(&database).unwrap();
            let write_txn = database.begin_write().unwrap();
            {
                let mut table = write_txn.open_table(DOCUMENT_PREFERENCES_V2_TABLE).unwrap();
                let key = history_key(&document_path);
                table
                    .insert(
                        key.as_slice(),
                        encode(&stored_preferences).unwrap().as_slice(),
                    )
                    .unwrap();
            }
            {
                let mut migrations = write_txn.open_table(STORAGE_MIGRATIONS_TABLE).unwrap();
                migrations
                    .insert(
                        STORAGE_V1_MIGRATION_KEY,
                        [STORAGE_SCHEMA_VERSION].as_slice(),
                    )
                    .unwrap();
                migrations
                    .insert(STORAGE_V2_MIGRATION_KEY, [STORAGE_V2_VERSION].as_slice())
                    .unwrap();
            }
            write_txn.commit().unwrap();
        }

        {
            let store = LibraryStore::open(database_path.clone()).unwrap();
            assert_eq!(
                store.document_target_language(&document_path).as_deref(),
                Some("ja")
            );
            assert_eq!(store.epub_reader_preferences(&document_path), Some(epub));
            assert_eq!(
                store.document_rotation(&document_path),
                DocumentRotation::default()
            );
        }

        {
            let database = Database::open(&database_path).unwrap();
            let read_txn = database.begin_read().unwrap();
            assert!(
                read_txn
                    .open_table(STORAGE_MIGRATIONS_TABLE)
                    .unwrap()
                    .get(DOCUMENT_PREFERENCES_V3_MIGRATION_KEY)
                    .unwrap()
                    .is_some()
            );
            let key = history_key(&document_path);
            let value = read_txn
                .open_table(DOCUMENT_PREFERENCES_V3_TABLE)
                .unwrap()
                .get(key.as_slice())
                .unwrap()
                .unwrap();
            let migrated = decode::<StoredDocumentPreferencesV3>(value.value()).unwrap();
            assert_eq!(migrated.target_language, "ja");
            assert_eq!(migrated.epub, Some(epub));
            assert_eq!(migrated.rotation, DocumentRotation::default());
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
            rotation: DocumentRotation::default(),
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
                        references: Vec::new(),
                        occurrences: Vec::new(),
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
                references: Vec::new(),
                occurrences: Vec::new(),
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
                    .insert(
                        session.id,
                        encode(&LegacyAiSession {
                            id: session.id,
                            path: session.path.clone(),
                            title: session.title.clone(),
                            page: session.page,
                            messages: session
                                .messages
                                .iter()
                                .map(|message| StoredAiSessionMessageV1 {
                                    role: message.role,
                                    content: message.content.clone(),
                                    request_content: message.request_content.clone(),
                                })
                                .collect(),
                            created: session.created,
                            updated: session.updated,
                        })
                        .unwrap()
                        .as_slice(),
                    )
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
