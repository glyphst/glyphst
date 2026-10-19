//! The library's search (frame 04n): the index that follows the library on
//! its own thread, and the searches, answered on another so typing never
//! waits. Answers carry the number of the search they answer; a stale one
//! is dropped.

use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
};

use anyhow::Result;
use glyphst_library::LibraryStore;
use glyphst_search::{
    IndexStatus, Indexer, ItemText, LibraryFile, SearchRequest, SearchResults, Snapshot, Store,
};

/// Items added within this long are read first.
const NEW_ITEM_SECONDS: u64 = 24 * 3_600;

pub struct SearchService {
    indexer: Indexer,
    queries: Sender<(u64, SearchRequest)>,
    answers: Receiver<(u64, Result<SearchResults, String>)>,
    /// The last search asked, and the answer to it once it came.
    asked: u64,
    answered: u64,
    request: Option<SearchRequest>,
    results: Option<SearchResults>,
    followed: Option<u64>,
}

impl SearchService {
    /// The index in the data folder.
    pub fn default_path() -> PathBuf {
        glyphst_core::paths::data_dir()
            .join("search-v1")
            .join("search.sqlite")
    }

    pub fn start(path: &Path, ctx: &egui::Context) -> Result<Self> {
        let notify = ctx.clone();
        let indexer = Indexer::start(path.to_path_buf(), move || notify.request_repaint())?;
        let (queries, receiver) = mpsc::channel::<(u64, SearchRequest)>();
        let (answer, answers) = mpsc::channel();
        let path = path.to_path_buf();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("search-query".to_string())
            .spawn(move || {
                let mut store = None;
                while let Ok(mut query) = receiver.recv() {
                    // Only the newest of the searches waiting.
                    while let Ok(newer) = receiver.try_recv() {
                        query = newer;
                    }
                    if store.is_none() {
                        store = Store::open_reader(&path).ok();
                    }
                    let result = match &store {
                        Some(store) => store.search(&query.1).map_err(|error| format!("{error:#}")),
                        None => Err("the search index is not ready".to_string()),
                    };
                    if answer.send((query.0, result)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            })?;
        Ok(Self {
            indexer,
            queries,
            answers,
            asked: 0,
            answered: 0,
            request: None,
            results: None,
            followed: None,
        })
    }

    /// Hands the index the library as it is, when it changed.
    pub fn follow(&mut self, library: &LibraryStore) {
        let revision = library.catalog_revision();
        if self.followed == Some(revision) {
            return;
        }
        self.followed = Some(revision);
        self.indexer.reconcile(snapshot(library));
    }

    /// Searches, unless this was the last search.
    pub fn search(&mut self, request: SearchRequest) {
        if self.request.as_ref().is_some_and(|asked| {
            asked.text == request.text
                && asked.items == request.items
                && asked.typing == request.typing
        }) {
            return;
        }
        self.asked += 1;
        self.request = Some(request.clone());
        let _ = self.queries.send((self.asked, request));
    }

    /// Takes the answers that came.
    pub fn poll(&mut self) {
        while let Ok((asked, result)) = self.answers.try_recv() {
            if asked != self.asked {
                continue;
            }
            self.answered = asked;
            match result {
                Ok(results) => self.results = Some(results),
                Err(error) => log::warn!("library search: {error}"),
            }
        }
    }

    /// The answer to the last search, or the one before while it comes.
    pub fn results(&self) -> Option<&SearchResults> {
        self.results.as_ref()
    }

    /// Whether the shown results answer an older search.
    pub fn is_stale(&self) -> bool {
        self.answered != self.asked
    }

    pub fn status(&self) -> IndexStatus {
        self.indexer.status()
    }

    pub fn set_paused(&self, paused: bool) {
        self.indexer.set_paused(paused);
    }
}

/// The library's items and files, as the index follows them.
pub fn snapshot(library: &LibraryStore) -> Snapshot {
    let now = glyphst_library::current_timestamp();
    let mut snapshot = Snapshot::default();
    for item in library.library_items() {
        let new = now.saturating_sub(item.created) < NEW_ITEM_SECONDS;
        snapshot.items.push(ItemText {
            item_id: item.id.0,
            title: item.title.clone(),
            creators: item
                .creators
                .iter()
                .map(|creator| creator.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            abstract_text: item.abstract_text.clone(),
            tags: library
                .tags_for_item(item.id)
                .iter()
                .map(|tag| tag.name.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            note: item.work_note.clone(),
            trashed: item.trashed_at.is_some(),
        });
        for attachment in library
            .library_attachments()
            .iter()
            .filter(|attachment| attachment.item_id == item.id)
        {
            snapshot.files.push(LibraryFile {
                item_id: item.id.0,
                attachment_id: attachment.id.0,
                path: attachment.path.clone(),
                sha256: attachment.content_sha256,
                size: attachment.file_size,
                priority: i64::from(new),
            });
        }
    }
    snapshot
}
