//! The search index: one SQLite file beside the library. It holds each
//! document's text in chunks that know their page, two full-text indexes
//! over them (words, and trigrams for Chinese, Japanese and Korean), the
//! items' own text, and, for meaning search, a vector per chunk.
//!
//! A file of another schema version is never migrated: it is renamed aside
//! (`search.sqlite.v<k>`) and a new index is built, as the library does.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use rusqlite::{Connection, OpenFlags};

/// The schema this build reads and writes.
pub const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

-- One row per distinct file content; items that share a file share it.
CREATE TABLE docs (
    id INTEGER PRIMARY KEY,
    sha256 BLOB NOT NULL UNIQUE,
    path TEXT NOT NULL,
    format TEXT NOT NULL,
    size INTEGER NOT NULL DEFAULT 0,
    page_count INTEGER NOT NULL DEFAULT 0,
    extractor INTEGER NOT NULL,
    -- queued | reading | done | failed
    state TEXT NOT NULL,
    -- Where reading resumes: the next page, and the glyphs before it.
    pages_done INTEGER NOT NULL DEFAULT 0,
    glyphs_done INTEGER NOT NULL DEFAULT 0,
    priority INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    retry_at INTEGER,
    error TEXT,
    updated INTEGER NOT NULL
);

-- The library's items: in the trash or not, and a hash of their text to
-- see when it changed.
CREATE TABLE items (
    item_id INTEGER PRIMARY KEY,
    trashed INTEGER NOT NULL,
    hash INTEGER NOT NULL
);
CREATE TABLE item_files (
    item_id INTEGER NOT NULL,
    attachment_id INTEGER NOT NULL,
    doc_id INTEGER NOT NULL,
    PRIMARY KEY (item_id, attachment_id)
);
CREATE INDEX item_files_doc ON item_files (doc_id);

-- Pages with almost no text are for image embedding or OCR later.
CREATE TABLE pages (
    doc_id INTEGER NOT NULL,
    page INTEGER NOT NULL,
    chars INTEGER NOT NULL,
    image_only INTEGER NOT NULL,
    PRIMARY KEY (doc_id, page)
);

-- About 1,800 characters of one page. The anchor (document-wide glyph
-- offset, quote, prefix, suffix) finds it again in any EPUB layout.
CREATE TABLE chunks (
    id INTEGER PRIMARY KEY,
    doc_id INTEGER NOT NULL,
    ord INTEGER NOT NULL,
    page INTEGER NOT NULL,
    glyph_start INTEGER NOT NULL,
    glyph_end INTEGER NOT NULL,
    doc_offset INTEGER NOT NULL,
    quote TEXT NOT NULL,
    prefix TEXT NOT NULL,
    suffix TEXT NOT NULL,
    -- The first line's boxes in page space, as little-endian f32 quads.
    rects BLOB,
    section TEXT NOT NULL DEFAULT '',
    text TEXT NOT NULL
);
CREATE INDEX chunks_doc ON chunks (doc_id, ord);

CREATE VIRTUAL TABLE chunks_fts USING fts5 (
    text, section,
    content = 'chunks', content_rowid = 'id',
    tokenize = 'porter unicode61 remove_diacritics 2'
);
-- Only chunks with Chinese, Japanese or Korean text, which unicode61 does
-- not split into words.
CREATE VIRTUAL TABLE chunks_tri USING fts5 (
    text,
    content = 'chunks', content_rowid = 'id',
    tokenize = 'trigram'
);
-- The items' own text (rowid: the item), so an item without a file is
-- found too.
CREATE VIRTUAL TABLE items_fts USING fts5 (
    title, creators, abstract, tags, note,
    tokenize = 'porter unicode61 remove_diacritics 2'
);

-- Meaning search: a vector per chunk and model (truncated, normalized,
-- float16), and how far each document is.
CREATE TABLE vectors (
    chunk_id INTEGER NOT NULL,
    model_key TEXT NOT NULL,
    vec BLOB NOT NULL,
    title TEXT NOT NULL,
    PRIMARY KEY (chunk_id, model_key)
);
CREATE TABLE embed_state (
    doc_id INTEGER NOT NULL,
    model_key TEXT NOT NULL,
    chunks_done INTEGER NOT NULL,
    PRIMARY KEY (doc_id, model_key)
);

-- How long work takes on this machine, learned as it goes.
CREATE TABLE estimates (
    kind TEXT PRIMARY KEY,
    ms_per_unit REAL NOT NULL,
    samples INTEGER NOT NULL
);
"#;

/// The index's file.
pub struct Store {
    pub(crate) conn: Connection,
}

impl Store {
    /// Opens the index at `path`, making it when missing. A file of another
    /// schema version is moved aside and a new one started.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        if path.exists() {
            let version = read_version(path);
            if version != Some(SCHEMA_VERSION) && version != Some(0) {
                set_aside(path, version)?;
            }
        }
        let conn =
            Connection::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        Self::prepare(conn)
    }

    /// A read-only connection to an index another thread writes.
    pub fn open_reader(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("failed to open {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version == 0 {
            conn.execute_batch(&format!(
                "BEGIN; {SCHEMA} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
            ))
            .context("failed to create the search index")?;
        }
        Ok(Self { conn })
    }

    /// How many bytes the index takes on disk.
    pub fn size_on_disk(path: &Path) -> u64 {
        ["", "-wal", "-shm"]
            .iter()
            .filter_map(|suffix| std::fs::metadata(with_suffix(path, suffix)).ok())
            .map(|metadata| metadata.len())
            .sum()
    }
}

fn read_version(path: &Path) -> Option<i64> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .ok()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Renames an index this build cannot read, with its journal files.
fn set_aside(path: &Path, version: Option<i64>) -> Result<()> {
    let tag = version.map_or_else(|| "unreadable".to_string(), |version| format!("v{version}"));
    for suffix in ["", "-wal", "-shm"] {
        let from = with_suffix(path, suffix);
        if from.exists() {
            let to = with_suffix(path, &format!(".{tag}{suffix}"));
            std::fs::rename(&from, &to)
                .with_context(|| format!("failed to move {} aside", from.display()))?;
        }
    }
    log::warn!(
        "the search index at {} is from another version; it was moved aside and is rebuilt",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_index_of_another_version_is_moved_aside() {
        let dir = std::env::temp_dir().join(format!("glyphst-search-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("search.sqlite");
        {
            let store = Store::open(&path).unwrap();
            let version: i64 = store
                .conn
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(version, SCHEMA_VERSION);
            store.conn.pragma_update(None, "user_version", 99).unwrap();
        }
        let store = Store::open(&path).unwrap();
        let version: i64 = store
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION, "a new index");
        assert!(dir.join("search.sqlite.v99").exists(), "the old one kept");
        drop(store);
        let _ = std::fs::remove_dir_all(dir);
    }
}
