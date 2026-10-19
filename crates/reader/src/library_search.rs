//! The library's search (frame 04n): the index that follows the library on
//! its own thread, and the searches, answered on another so typing never
//! waits. Answers carry the number of the search they answer; a stale one
//! is dropped.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use glyphst_library::LibraryStore;
use glyphst_search::{
    IndexStatus, Indexer, ItemText, LibraryFile, SearchRequest, SearchResults, Snapshot, Store,
    embed::Embedder, search::Meaning, vectors::VectorIndex,
};

/// Items added within this long are read first.
const NEW_ITEM_SECONDS: u64 = 24 * 3_600;
/// The meaning pass waits for typing to stop this long (DigUp's).
const TYPING_PAUSE: Duration = Duration::from_millis(120);
/// The vectors in memory are loaded again at most this often while the
/// index grows.
const RELOAD_EVERY: Duration = Duration::from_secs(10);

enum Query {
    Search {
        asked: u64,
        request: SearchRequest,
        meaning: bool,
    },
    Embedder(Option<Arc<dyn Embedder>>),
}

struct Answer {
    asked: u64,
    meaning: bool,
    result: Result<SearchResults, String>,
}

pub struct SearchService {
    indexer: Indexer,
    path: PathBuf,
    queries: Sender<Query>,
    answers: Receiver<Answer>,
    /// The last search asked, and the answer to it once it came.
    asked: u64,
    answered: u64,
    request: Option<SearchRequest>,
    results: Option<SearchResults>,
    followed: Option<u64>,
    /// A reader for finding passages from the UI (one row by its key).
    locator: Option<Store>,
    /// Search by meaning: the model, when it runs; when the last search
    /// changed; whether its meaning pass went out, and came back.
    meaning: bool,
    changed_at: Instant,
    meaning_sent: bool,
    meaning_answered: bool,
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
        let (queries, receiver) = mpsc::channel::<Query>();
        let (answer, answers) = mpsc::channel();
        let index_path = path.to_path_buf();
        let path = path.to_path_buf();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("search-query".to_string())
            .spawn(move || {
                let mut store = None;
                let mut embedder: Option<Arc<dyn Embedder>> = None;
                let mut vectors: Option<(VectorIndex, Instant)> = None;
                while let Ok(first) = receiver.recv() {
                    // The model as it now is, and only the newest search.
                    let mut search = None;
                    for query in std::iter::once(first).chain(receiver.try_iter()) {
                        match query {
                            Query::Embedder(next) => {
                                embedder = next;
                                vectors = None;
                            }
                            Query::Search { .. } => search = Some(query),
                        }
                    }
                    let Some(Query::Search {
                        asked,
                        request,
                        meaning,
                    }) = search
                    else {
                        continue;
                    };
                    if store.is_none() {
                        store = Store::open_reader(&path).ok();
                    }
                    let Some(store) = &store else {
                        let _ = answer.send(Answer {
                            asked,
                            meaning,
                            result: Err("the search index is not ready".to_string()),
                        });
                        continue;
                    };
                    let mut result = Err(String::new());
                    if meaning && let Some(embedder) = &embedder {
                        // The vectors, loaded again as the index grows.
                        let stale = vectors
                            .as_ref()
                            .is_none_or(|(_, loaded)| loaded.elapsed() > RELOAD_EVERY);
                        if stale {
                            match &mut vectors {
                                Some((index, loaded)) => {
                                    let _ = index.refresh(store);
                                    *loaded = Instant::now();
                                }
                                None => {
                                    vectors = VectorIndex::load(store, embedder.model_key())
                                        .ok()
                                        .map(|index| (index, Instant::now()));
                                }
                            }
                        }
                        if let (Some((index, _)), Ok(vector)) =
                            (&vectors, embedder.embed_query(&request.text))
                        {
                            result = store
                                .search_with(
                                    &request,
                                    Some(Meaning {
                                        index,
                                        query: &vector,
                                    }),
                                )
                                .map_err(|error| format!("{error:#}"));
                        }
                    }
                    if result.is_err() {
                        result = store.search(&request).map_err(|error| format!("{error:#}"));
                    }
                    if answer
                        .send(Answer {
                            asked,
                            meaning,
                            result,
                        })
                        .is_err()
                    {
                        return;
                    }
                    ctx.request_repaint();
                }
            })?;
        Ok(Self {
            indexer,
            path: index_path,
            queries,
            answers,
            asked: 0,
            answered: 0,
            request: None,
            results: None,
            followed: None,
            locator: None,
            meaning: false,
            changed_at: Instant::now(),
            meaning_sent: false,
            meaning_answered: false,
        })
    }

    /// Search by meaning with `embedder` (the index embeds every passage,
    /// and searches get a meaning pass), or stop it.
    pub fn set_embedder(&mut self, embedder: Option<Arc<dyn Embedder>>) {
        self.meaning = embedder.is_some();
        self.indexer.set_embedder(embedder.clone());
        let _ = self.queries.send(Query::Embedder(embedder));
        // The last search again, now with its meaning pass.
        self.meaning_sent = false;
        self.meaning_answered = false;
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

    /// Searches by keyword now; the meaning pass follows once typing stops
    /// (`poll`). Nothing when this was the last search.
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
        self.changed_at = Instant::now();
        self.meaning_sent = false;
        self.meaning_answered = false;
        let _ = self.queries.send(Query::Search {
            asked: self.asked,
            request,
            meaning: false,
        });
    }

    /// Takes the answers that came, and sends the meaning pass once typing
    /// stopped.
    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(answer) = self.answers.try_recv() {
            // A keyword answer never replaces its meaning pass.
            if answer.asked != self.asked || (!answer.meaning && self.meaning_answered) {
                continue;
            }
            self.answered = answer.asked;
            self.meaning_answered |= answer.meaning;
            match answer.result {
                Ok(results) => self.results = Some(results),
                Err(error) => log::warn!("library search: {error}"),
            }
        }
        if self.meaning
            && !self.meaning_sent
            && let Some(request) = &self.request
        {
            let waited = self.changed_at.elapsed();
            if waited >= TYPING_PAUSE {
                self.meaning_sent = true;
                let _ = self.queries.send(Query::Search {
                    asked: self.asked,
                    request: SearchRequest {
                        typing: false,
                        ..request.clone()
                    },
                    meaning: true,
                });
            } else {
                ctx.request_repaint_after(TYPING_PAUSE - waited);
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

    pub fn is_paused(&self) -> bool {
        self.indexer.is_paused()
    }

    pub fn reindex(&self) {
        self.indexer.reindex();
    }

    /// Where a passage is, for a link to it.
    pub fn locate(&mut self, chunk: i64) -> Option<glyphst_search::ChunkLocation> {
        if self.locator.is_none() {
            self.locator = Store::open_reader(&self.path).ok();
        }
        self.locator.as_ref()?.chunk(chunk).ok().flatten()
    }

    /// The index's file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// `about 12 min left`, from the index's learned speed.
pub fn time_left(status: &IndexStatus) -> Option<String> {
    let seconds = status.seconds_left()?;
    Some(match seconds {
        0..60 => "under a minute left".to_string(),
        60..3_600 => format!("about {} min left", seconds.div_ceil(60)),
        _ => format!("about {:.1} h left", seconds as f64 / 3_600.0),
    })
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use glyphst_pdf::test_pdf::text_pages_pdf;

    use super::*;

    /// The real engine end to end, by hand: GLYPHST_EMBED_SERVER,
    /// GLYPHST_EMBED_MODEL (and GLYPHST_EMBED_LIBS on NixOS), then
    /// `cargo test -p glyphst-reader real_meaning -- --ignored`.
    #[test]
    #[ignore = "needs the real engine and model"]
    fn real_meaning_search_finds_a_paraphrase() {
        let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
        let (Some(exe), Some(model)) = (var("GLYPHST_EMBED_SERVER"), var("GLYPHST_EMBED_MODEL"))
        else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("glyphst-real-meaning-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let texts = [
            "Selective state space models scale linearly with sequence length",
            "A collection of recipes for baking bread at home",
            "Convolutional networks classify images of handwritten digits",
        ];
        let mut snapshot = Snapshot::default();
        for (index, text) in texts.iter().enumerate() {
            let path = dir.join(format!("{index}.pdf"));
            std::fs::write(&path, text_pages_pdf(&[&[text]])).unwrap();
            let id = index as u64 + 1;
            snapshot.files.push(LibraryFile {
                item_id: id,
                attachment_id: id,
                size: std::fs::metadata(&path).unwrap().len(),
                path,
                sha256: [index as u8 + 1; 32],
                priority: 0,
            });
            snapshot.items.push(ItemText {
                item_id: id,
                title: format!("paper {id}"),
                ..ItemText::default()
            });
        }
        let ctx = egui::Context::default();
        let mut service = SearchService::start(&dir.join("search.sqlite"), &ctx).unwrap();
        service.indexer.reconcile(snapshot);
        let embedder =
            glyphst_embed::llama::LlamaEmbedder::start(&glyphst_embed::server::ServerConfig {
                exe,
                model,
                library_dirs: var("GLYPHST_EMBED_LIBS").into_iter().collect(),
                pid_file: None,
            })
            .unwrap();
        service.set_embedder(Some(Arc::new(embedder)));
        let started = Instant::now();
        while service.status().embedded < texts.len() {
            assert!(started.elapsed().as_secs() < 120, "{:?}", service.status());
            std::thread::sleep(Duration::from_millis(100));
        }
        // No word in common with the first paper.
        service.search(SearchRequest {
            text: "efficient models for very long inputs".to_string(),
            ..SearchRequest::default()
        });
        let started = Instant::now();
        loop {
            service.poll(&ctx);
            if service.meaning_answered {
                break;
            }
            assert!(started.elapsed().as_secs() < 60);
            std::thread::sleep(Duration::from_millis(20));
        }
        let results = service.results().unwrap();
        eprintln!("{results:#?}");
        assert_eq!(results.items[0].item_id, 1);
        assert!(results.items[0].pages[0].meaning.unwrap() > 0.0);
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }
}
