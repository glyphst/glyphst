//! Writing the index: following the library (which items have which
//! files), choosing the next document to read, and saving each step so an
//! interrupted read resumes where it stopped.

use std::{
    collections::HashSet,
    hash::{DefaultHasher, Hash as _, Hasher as _},
    path::PathBuf,
};

use anyhow::Result;
use rusqlite::{OptionalExtension as _, Transaction, params};

use crate::{
    chunk::{Chunk, PageChunks},
    extract::EXTRACTOR_VERSION,
    store::Store,
};

/// Waits before a failed document is tried again; then it stays failed.
const RETRY_AFTER: [i64; 3] = [60, 600, 3_600];

/// One of an item's files, as the library has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryFile {
    pub item_id: u64,
    pub attachment_id: u64,
    pub path: PathBuf,
    pub sha256: [u8; 32],
    pub size: u64,
    /// Higher goes first: a new item, the open document.
    pub priority: i64,
}

/// An item's own text, searched beside its files.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ItemText {
    pub item_id: u64,
    pub title: String,
    pub creators: String,
    pub abstract_text: String,
    pub tags: String,
    pub note: String,
    pub trashed: bool,
}

/// The library as the index follows it.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub files: Vec<LibraryFile>,
    pub items: Vec<ItemText>,
}

/// A document to read next, and where to resume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocJob {
    pub doc_id: i64,
    pub path: PathBuf,
    pub pages_done: usize,
    pub glyphs_done: usize,
    pub attempts: u32,
}

/// How far the index is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexStatus {
    pub docs: usize,
    pub done: usize,
    pub failed: usize,
    pub pages: usize,
    pub pages_done: usize,
    pub chunks: usize,
}

impl IndexStatus {
    pub fn waiting(&self) -> usize {
        self.docs - self.done - self.failed
    }
}

/// Whether text has Chinese, Japanese or Korean in it, which the word index
/// does not split.
pub fn has_cjk(text: &str) -> bool {
    text.chars().any(is_cjk)
}

pub fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3040..=0x30FF       // kana
        | 0x3400..=0x4DBF     // CJK extension A
        | 0x4E00..=0x9FFF     // CJK unified
        | 0xAC00..=0xD7AF     // Hangul
        | 0xF900..=0xFAFF     // compatibility
        | 0x20000..=0x2FA1F)
}

impl Store {
    /// Follows the library: new contents are queued, moved files keep their
    /// text, items' own text is updated, and documents no item has any more
    /// are dropped. A document read by an older extractor is read again.
    pub fn reconcile(&mut self, snapshot: &Snapshot, now: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        reconcile_items(&tx, &snapshot.items)?;
        let mut kept = HashSet::new();
        for file in &snapshot.files {
            let path = file.path.to_string_lossy();
            let existing: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT id, priority FROM docs WHERE sha256 = ?1",
                    [file.sha256.as_slice()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let doc_id = match existing {
                Some((id, priority)) => {
                    tx.execute(
                        "UPDATE docs SET path = ?2, priority = ?3 WHERE id = ?1",
                        params![id, path, priority.max(file.priority)],
                    )?;
                    id
                }
                None => {
                    tx.execute(
                        "INSERT INTO docs (sha256, path, format, size, extractor, state, priority, updated)
                         VALUES (?1, ?2, '', ?3, ?4, 'queued', ?5, ?6)",
                        params![
                            file.sha256.as_slice(),
                            path,
                            file.size as i64,
                            EXTRACTOR_VERSION,
                            file.priority,
                            now
                        ],
                    )?;
                    tx.last_insert_rowid()
                }
            };
            tx.execute(
                "INSERT INTO item_files (item_id, attachment_id, doc_id) VALUES (?1, ?2, ?3)
                 ON CONFLICT (item_id, attachment_id) DO UPDATE SET doc_id = excluded.doc_id",
                params![file.item_id as i64, file.attachment_id as i64, doc_id],
            )?;
            kept.insert((file.item_id, file.attachment_id));
        }
        // Files the library no longer has.
        let stale = {
            let mut statement = tx.prepare("SELECT item_id, attachment_id FROM item_files")?;
            statement
                .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter(|(item, attachment)| !kept.contains(&(*item as u64, *attachment as u64)))
                .collect::<Vec<_>>()
        };
        for (item, attachment) in stale {
            tx.execute(
                "DELETE FROM item_files WHERE item_id = ?1 AND attachment_id = ?2",
                params![item, attachment],
            )?;
        }
        // Documents nothing refers to, and ones an older extractor read.
        let orphans = ids(
            &tx,
            "SELECT id FROM docs WHERE id NOT IN (SELECT doc_id FROM item_files)",
        )?;
        for doc_id in orphans {
            delete_doc_text(&tx, doc_id)?;
            tx.execute("DELETE FROM docs WHERE id = ?1", [doc_id])?;
        }
        let outdated = ids(
            &tx,
            &format!("SELECT id FROM docs WHERE extractor < {EXTRACTOR_VERSION}"),
        )?;
        for doc_id in outdated {
            delete_doc_text(&tx, doc_id)?;
            tx.execute(
                "UPDATE docs SET extractor = ?2, state = 'queued', pages_done = 0,
                 glyphs_done = 0, attempts = 0, retry_at = NULL, error = NULL WHERE id = ?1",
                params![doc_id, EXTRACTOR_VERSION],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The document to read next: the highest priority, then the smallest,
    /// so early on many documents are searchable.
    pub fn next_doc(&self, now: i64) -> Result<Option<DocJob>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, path, pages_done, glyphs_done, attempts FROM docs
                 WHERE state IN ('queued', 'reading')
                    OR (state = 'failed' AND retry_at IS NOT NULL AND retry_at <= ?1)
                 ORDER BY priority DESC, size ASC, id ASC LIMIT 1",
                [now],
                |row| {
                    Ok(DocJob {
                        doc_id: row.get(0)?,
                        path: PathBuf::from(row.get::<_, String>(1)?),
                        pages_done: row.get::<_, i64>(2)? as usize,
                        glyphs_done: row.get::<_, i64>(3)? as usize,
                        attempts: row.get::<_, i64>(4)? as u32,
                    })
                },
            )
            .optional()?)
    }

    /// When the next failed document may be tried again, if one may.
    pub fn next_retry(&self) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT MIN(retry_at) FROM docs WHERE state = 'failed' AND retry_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?)
    }

    /// Saves a step of reading: these pages' chunks, and where to go on.
    /// `pages_done` reaching `page_count` finishes the document.
    pub fn save_step(&mut self, step: &Step<'_>, now: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        let mut ord: i64 = tx.query_row(
            "SELECT COALESCE(MAX(ord) + 1, 0) FROM chunks WHERE doc_id = ?1",
            [step.doc_id],
            |row| row.get(0),
        )?;
        for (page, chunks) in step.pages {
            // A page read twice (a step cut short) replaces itself.
            let old = ids(
                &tx,
                &format!(
                    "SELECT id FROM chunks WHERE doc_id = {} AND page = {page}",
                    step.doc_id
                ),
            )?;
            for chunk_id in old {
                delete_chunk(&tx, chunk_id)?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO pages (doc_id, page, chars, image_only) VALUES (?1, ?2, ?3, ?4)",
                params![step.doc_id, *page as i64, chunks.chars as i64, chunks.image_only],
            )?;
            for chunk in &chunks.chunks {
                insert_chunk(&tx, step.doc_id, ord, chunk)?;
                ord += 1;
            }
        }
        let finished = step.pages_done >= step.page_count;
        tx.execute(
            "UPDATE docs SET format = ?2, page_count = ?3, pages_done = ?4, glyphs_done = ?5,
             state = ?6, attempts = 0, retry_at = NULL, error = NULL, updated = ?7 WHERE id = ?1",
            params![
                step.doc_id,
                step.format,
                step.page_count as i64,
                step.pages_done as i64,
                step.glyphs_done as i64,
                if finished { "done" } else { "reading" },
                now
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Marks a document failed; it is tried again later, three times.
    pub fn fail_doc(&mut self, doc_id: i64, error: &str, now: i64) -> Result<()> {
        let attempts: i64 =
            self.conn
                .query_row("SELECT attempts FROM docs WHERE id = ?1", [doc_id], |row| {
                    row.get(0)
                })?;
        let retry_at = RETRY_AFTER.get(attempts as usize).map(|wait| now + wait);
        self.conn.execute(
            "UPDATE docs SET state = 'failed', attempts = ?2, retry_at = ?3, error = ?4, updated = ?5
             WHERE id = ?1",
            params![doc_id, attempts + 1, retry_at, error, now],
        )?;
        Ok(())
    }

    /// Reads every document again (Settings › re-index).
    pub fn reindex_all(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        for doc_id in ids(&tx, "SELECT id FROM docs")? {
            delete_doc_text(&tx, doc_id)?;
        }
        tx.execute(
            "UPDATE docs SET state = 'queued', pages_done = 0, glyphs_done = 0, attempts = 0,
             retry_at = NULL, error = NULL",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn status(&self) -> Result<IndexStatus> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(state = 'done'), 0),
                    COALESCE(SUM(state = 'failed' AND retry_at IS NULL), 0),
                    COALESCE(SUM(page_count), 0),
                    COALESCE(SUM(pages_done), 0),
                    (SELECT COUNT(*) FROM chunks)
             FROM docs",
            [],
            |row| {
                Ok(IndexStatus {
                    docs: row.get::<_, i64>(0)? as usize,
                    done: row.get::<_, i64>(1)? as usize,
                    failed: row.get::<_, i64>(2)? as usize,
                    pages: row.get::<_, i64>(3)? as usize,
                    pages_done: row.get::<_, i64>(4)? as usize,
                    chunks: row.get::<_, i64>(5)? as usize,
                })
            },
        )?)
    }

    /// The document with this content, if the index has it.
    pub fn doc_id(&self, sha256: &[u8; 32]) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM docs WHERE sha256 = ?1",
                [sha256.as_slice()],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// The path the index last saw for a file, by its content hash.
    pub fn doc_path(&self, sha256: &[u8; 32]) -> Result<Option<PathBuf>> {
        Ok(self
            .conn
            .query_row(
                "SELECT path FROM docs WHERE sha256 = ?1",
                [sha256.as_slice()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(PathBuf::from))
    }
}

/// One step of reading a document.
pub struct Step<'a> {
    pub doc_id: i64,
    pub format: &'a str,
    pub page_count: usize,
    pub pages: &'a [(usize, PageChunks)],
    /// Where the next step starts: a page, and the glyphs before it.
    pub pages_done: usize,
    pub glyphs_done: usize,
}

fn reconcile_items(tx: &Transaction<'_>, items: &[ItemText]) -> Result<()> {
    let mut present = HashSet::new();
    for item in items {
        present.insert(item.item_id as i64);
        let hash = {
            let mut hasher = DefaultHasher::new();
            item.hash(&mut hasher);
            hasher.finish() as i64
        };
        let old: Option<i64> = tx
            .query_row(
                "SELECT hash FROM items WHERE item_id = ?1",
                [item.item_id as i64],
                |row| row.get(0),
            )
            .optional()?;
        if old == Some(hash) {
            continue;
        }
        tx.execute(
            "INSERT OR REPLACE INTO items (item_id, trashed, hash) VALUES (?1, ?2, ?3)",
            params![item.item_id as i64, item.trashed, hash],
        )?;
        tx.execute(
            "DELETE FROM items_fts WHERE rowid = ?1",
            [item.item_id as i64],
        )?;
        tx.execute(
            "INSERT INTO items_fts (rowid, title, creators, abstract, tags, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                item.item_id as i64,
                item.title,
                item.creators,
                item.abstract_text,
                item.tags,
                item.note
            ],
        )?;
    }
    for item_id in ids(tx, "SELECT item_id FROM items")? {
        if !present.contains(&item_id) {
            tx.execute("DELETE FROM items WHERE item_id = ?1", [item_id])?;
            tx.execute("DELETE FROM items_fts WHERE rowid = ?1", [item_id])?;
        }
    }
    Ok(())
}

fn ids(tx: &Transaction<'_>, sql: &str) -> Result<Vec<i64>> {
    let mut statement = tx.prepare(sql)?;
    let ids = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(ids)
}

fn insert_chunk(tx: &Transaction<'_>, doc_id: i64, ord: i64, chunk: &Chunk) -> Result<()> {
    let rects = chunk
        .rects
        .iter()
        .flat_map(|rect| rect.iter().flat_map(|value| value.to_le_bytes()))
        .collect::<Vec<u8>>();
    tx.execute(
        "INSERT INTO chunks (doc_id, ord, page, glyph_start, glyph_end, doc_offset, quote, prefix,
         suffix, rects, section, text) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            doc_id,
            ord,
            chunk.page as i64,
            chunk.glyph_start as i64,
            chunk.glyph_end as i64,
            chunk.doc_offset as i64,
            chunk.quote,
            chunk.prefix,
            chunk.suffix,
            rects,
            chunk.section,
            chunk.text
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO chunks_fts (rowid, text, section) VALUES (?1, ?2, ?3)",
        params![id, chunk.text, chunk.section],
    )?;
    if has_cjk(&chunk.text) {
        tx.execute(
            "INSERT INTO chunks_tri (rowid, text) VALUES (?1, ?2)",
            params![id, chunk.text],
        )?;
    }
    Ok(())
}

/// Removes a chunk and its rows in the full-text indexes, which hold copies
/// of its text and must be told what they had ('delete').
fn delete_chunk(tx: &Transaction<'_>, chunk_id: i64) -> Result<()> {
    let (text, section): (String, String) = tx.query_row(
        "SELECT text, section FROM chunks WHERE id = ?1",
        [chunk_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    tx.execute(
        "INSERT INTO chunks_fts (chunks_fts, rowid, text, section) VALUES ('delete', ?1, ?2, ?3)",
        params![chunk_id, text, section],
    )?;
    if has_cjk(&text) {
        tx.execute(
            "INSERT INTO chunks_tri (chunks_tri, rowid, text) VALUES ('delete', ?1, ?2)",
            params![chunk_id, text],
        )?;
    }
    tx.execute("DELETE FROM vectors WHERE chunk_id = ?1", [chunk_id])?;
    tx.execute("DELETE FROM chunks WHERE id = ?1", [chunk_id])?;
    Ok(())
}

fn delete_doc_text(tx: &Transaction<'_>, doc_id: i64) -> Result<()> {
    for chunk_id in ids(
        tx,
        &format!("SELECT id FROM chunks WHERE doc_id = {doc_id}"),
    )? {
        delete_chunk(tx, chunk_id)?;
    }
    tx.execute("DELETE FROM pages WHERE doc_id = ?1", [doc_id])?;
    tx.execute("DELETE FROM embed_state WHERE doc_id = ?1", [doc_id])?;
    Ok(())
}
