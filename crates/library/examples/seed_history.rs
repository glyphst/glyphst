//! Writes the reading history a screenshot run starts from:
//! `seed_history DOCUMENT:PAGE…` records each document as read at PAGE
//! (from 1), an hour apart with the last one newest, in the library under
//! `$XDG_DATA_HOME`.

use std::path::Path;

use anyhow::{Context as _, Result};
use glyphst_library::{
    DocumentPosition, HistoryEntry, LibraryStore, current_timestamp, normalize_path,
};

fn main() -> Result<()> {
    let documents: Vec<String> = std::env::args().skip(1).collect();
    let now = current_timestamp();
    let mut history = Vec::new();
    for (index, document) in documents.iter().enumerate() {
        let (path, page) = document
            .rsplit_once(':')
            .with_context(|| format!("{document} is not DOCUMENT:PAGE"))?;
        let page: usize = page
            .parse()
            .with_context(|| format!("{page} is not a page number"))?;
        history.push(HistoryEntry {
            path: normalize_path(Path::new(path)),
            opened: now - 3_600 * (documents.len() - index) as u64,
            position: DocumentPosition::fixed(0.0, 0.0, 1.0, page.saturating_sub(1)),
        });
    }
    let mut store = LibraryStore::load();
    store.replace_history_entries(history)?;
    store.save()
}
