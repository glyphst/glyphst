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
    embed::{Embedder, Reranker},
    search::Meaning,
    vectors::VectorIndex,
};

/// Items added within this long are read first.
const NEW_ITEM_SECONDS: u64 = 24 * 3_600;
/// The meaning pass waits for typing to stop this long (DigUp's).
const TYPING_PAUSE: Duration = Duration::from_millis(120);
/// The vectors in memory are loaded again at most this often while the
/// index grows.
const RELOAD_EVERY: Duration = Duration::from_secs(10);
/// Items close in meaning are worked out again this often.
const NEIGHBOURS_EVERY: Duration = Duration::from_secs(30);
/// Related items suggested for each item, at most.
const NEIGHBOURS_PER_ITEM: usize = 5;

/// Items close in meaning (each with its closest, and their cosine), and
/// topics: groups of three or more that are each other's neighbours.
#[derive(Clone, Debug, Default)]
pub struct Neighbours {
    pub related: Vec<(u64, Vec<(u64, f32)>)>,
    pub topics: Vec<Vec<u64>>,
}

/// A search's passes, in the order they answer: each replaces the one
/// before and is never replaced by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pass {
    /// On every keystroke.
    Keyword,
    /// Once typing stops: with meaning.
    Meaning,
    /// After the meaning pass: the best passages reread.
    Rerank,
}

enum Query {
    Search {
        asked: u64,
        request: SearchRequest,
        pass: Pass,
    },
    Embedder(Option<Arc<dyn Embedder>>),
    Reranker(Option<Arc<dyn Reranker>>),
    /// The assistant's search: every pass that runs, answered to `reply`.
    Tool {
        request: SearchRequest,
        reply: Sender<Result<SearchResults, String>>,
    },
    /// Items close in meaning, from the vectors in memory.
    Neighbours(Sender<Neighbours>),
    /// The service is gone (the assistant's handles may outlive it).
    Stop,
}

/// How long the assistant's search waits for the query thread (the
/// meaning and rerank passes take a second or two).
const TOOL_WAIT: Duration = Duration::from_secs(60);

/// The assistant's way into the search service, from the agent's thread:
/// the same vectors and reranker as the library tab's searches.
#[derive(Clone)]
pub struct ToolSearcher(Sender<Query>);

impl ToolSearcher {
    pub fn search(&self, request: SearchRequest) -> Result<SearchResults> {
        let (reply, answer) = mpsc::channel();
        self.0
            .send(Query::Tool { request, reply })
            .map_err(|_| anyhow::anyhow!("the library's search stopped"))?;
        answer
            .recv_timeout(TOOL_WAIT)
            .map_err(|_| anyhow::anyhow!("the library's search did not answer"))?
            .map_err(|error| anyhow::anyhow!(error))
    }
}

struct Answer {
    asked: u64,
    pass: Pass,
    result: Result<SearchResults, String>,
}

/// What the query thread searches with.
#[derive(Default)]
struct Models {
    embedder: Option<Arc<dyn Embedder>>,
    reranker: Option<Arc<dyn Reranker>>,
    vectors: Option<(VectorIndex, Instant)>,
}

impl Models {
    /// `request` as far as `pass` goes, with what runs: a pass whose model
    /// does not run (or fails) falls back to the one before.
    /// The vectors, loaded again as the index grows.
    fn refresh_vectors(&mut self, store: &Store) {
        let Some(embedder) = &self.embedder else {
            return;
        };
        let stale = self
            .vectors
            .as_ref()
            .is_none_or(|(_, loaded)| loaded.elapsed() > RELOAD_EVERY);
        if stale {
            match &mut self.vectors {
                Some((index, loaded)) => {
                    let _ = index.refresh(store);
                    *loaded = Instant::now();
                }
                None => {
                    self.vectors = VectorIndex::load(store, embedder.model_key())
                        .ok()
                        .map(|index| (index, Instant::now()));
                }
            }
        }
    }

    /// Items close in meaning, while search by meaning runs.
    fn neighbours(&mut self, store: &Store) -> Neighbours {
        self.refresh_vectors(store);
        let Some((index, _)) = &self.vectors else {
            return Neighbours::default();
        };
        let Ok(owners) = store.chunk_owners() else {
            return Neighbours::default();
        };
        let items = index.item_means(&owners);
        Neighbours {
            related: items.neighbours(NEIGHBOURS_PER_ITEM),
            topics: items.clusters(NEIGHBOURS_PER_ITEM, 3),
        }
    }

    fn answer(
        &mut self,
        store: &Store,
        request: &SearchRequest,
        pass: Pass,
    ) -> Result<SearchResults> {
        let mut query = None;
        if pass >= Pass::Meaning
            && let Some(embedder) = self.embedder.clone()
        {
            self.refresh_vectors(store);
            match embedder.embed_query(&request.text) {
                Ok(vector) => query = Some(vector),
                Err(error) => log::warn!("library search: the query's meaning: {error:#}"),
            }
        }
        let vectors = &self.vectors;
        let meaning = || match (vectors, &query) {
            (Some((index, _)), Some(vector)) => Some(Meaning {
                index,
                query: vector,
            }),
            _ => None,
        };
        if pass >= Pass::Rerank
            && let Some(reranker) = &self.reranker
        {
            match store.search_reranked(request, meaning(), reranker.as_ref()) {
                Ok(results) => return Ok(results),
                Err(error) => log::warn!("library search: reranking: {error:#}"),
            }
        }
        store.search_with(request, meaning())
    }
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
    followed: Option<(u64, u64)>,
    /// A reader for finding passages from the UI (one row by its key).
    locator: Option<Store>,
    /// Search by meaning and the reranker, when they run; when the last
    /// search changed; how far its passes went out, and came back.
    meaning: bool,
    rerank: bool,
    changed_at: Instant,
    sent: Pass,
    shown: Pass,
    /// Items close in meaning, as last worked out; the request out, and when.
    neighbours: Option<Arc<Neighbours>>,
    neighbours_reply: Option<Receiver<Neighbours>>,
    neighbours_at: Option<Instant>,
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
                let mut models = Models::default();
                while let Ok(first) = receiver.recv() {
                    // The models as they now are, and only the newest search.
                    let mut search = None;
                    let mut tools = Vec::new();
                    let mut neighbours = Vec::new();
                    for query in std::iter::once(first).chain(receiver.try_iter()) {
                        match query {
                            Query::Embedder(next) => {
                                models.embedder = next;
                                models.vectors = None;
                            }
                            Query::Reranker(next) => models.reranker = next,
                            Query::Search { .. } => search = Some(query),
                            Query::Tool { request, reply } => tools.push((request, reply)),
                            Query::Neighbours(reply) => neighbours.push(reply),
                            Query::Stop => return,
                        }
                    }
                    if store.is_none() {
                        store = Store::open_reader(&path).ok();
                    }
                    if let (Some(store), false) = (&store, neighbours.is_empty()) {
                        let found = models.neighbours(store);
                        for reply in neighbours {
                            let _ = reply.send(found.clone());
                        }
                        ctx.request_repaint();
                    }
                    // Each of the assistant's searches is answered.
                    for (request, reply) in tools {
                        let result = match &store {
                            Some(store) => models
                                .answer(store, &request, Pass::Rerank)
                                .map_err(|error| format!("{error:#}")),
                            None => Err("the search index is not ready".to_string()),
                        };
                        let _ = reply.send(result);
                    }
                    let Some(Query::Search {
                        asked,
                        request,
                        pass,
                    }) = search
                    else {
                        continue;
                    };
                    let result = match &store {
                        Some(store) => models
                            .answer(store, &request, pass)
                            .map_err(|error| format!("{error:#}")),
                        None => Err("the search index is not ready".to_string()),
                    };
                    if answer
                        .send(Answer {
                            asked,
                            pass,
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
            rerank: false,
            changed_at: Instant::now(),
            sent: Pass::Keyword,
            shown: Pass::Keyword,
            neighbours: None,
            neighbours_reply: None,
            neighbours_at: None,
        })
    }

    /// Search by meaning with `embedder` (the index embeds every passage,
    /// and searches get a meaning pass), or stop it.
    pub fn set_embedder(&mut self, embedder: Option<Arc<dyn Embedder>>) {
        self.meaning = embedder.is_some();
        self.indexer.set_embedder(embedder.clone());
        let _ = self.queries.send(Query::Embedder(embedder));
        // The last search again, now with its meaning pass.
        self.sent = Pass::Keyword;
        self.shown = Pass::Keyword;
    }

    /// Rereads the best passages of each search with `reranker`, or stops.
    pub fn set_reranker(&mut self, reranker: Option<Arc<dyn Reranker>>) {
        self.rerank = reranker.is_some();
        let _ = self.queries.send(Query::Reranker(reranker));
        self.sent = self.sent.min(Pass::Meaning);
        self.shown = self.shown.min(Pass::Meaning);
        if !self.meaning {
            self.sent = Pass::Keyword;
        }
    }

    /// The last pass a search gets with what runs now.
    fn last_pass(&self) -> Pass {
        if self.rerank {
            Pass::Rerank
        } else if self.meaning {
            Pass::Meaning
        } else {
            Pass::Keyword
        }
    }

    /// Hands the index the library as it is, when it changed.
    /// Reads what changed in the library: its catalog, or files another
    /// computer added that have arrived (`arrived` counts them).
    pub fn follow(&mut self, library: &LibraryStore, arrived: u64) {
        let revision = (library.catalog_revision(), arrived);
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
        self.sent = Pass::Keyword;
        self.shown = Pass::Keyword;
        let _ = self.queries.send(Query::Search {
            asked: self.asked,
            request,
            pass: Pass::Keyword,
        });
    }

    /// Takes the answers that came; sends the meaning pass once typing
    /// stopped, and the rerank pass once that answered.
    pub fn poll(&mut self, ctx: &egui::Context) {
        self.poll_neighbours();
        while let Ok(answer) = self.answers.try_recv() {
            // A pass never replaces a later one.
            if answer.asked != self.asked
                || (answer.pass < self.shown && self.answered == self.asked)
            {
                continue;
            }
            self.answered = answer.asked;
            self.shown = answer.pass;
            match answer.result {
                Ok(results) => self.results = Some(results),
                Err(error) => log::warn!("library search: {error}"),
            }
        }
        let last = self.last_pass();
        let Some(request) = &self.request else {
            return;
        };
        if self.sent >= last {
            return;
        }
        let next = match self.sent {
            // Once typing stops: meaning, or straight to the reranker.
            Pass::Keyword => {
                let waited = self.changed_at.elapsed();
                if waited < TYPING_PAUSE {
                    ctx.request_repaint_after(TYPING_PAUSE - waited);
                    return;
                }
                if self.meaning {
                    Pass::Meaning
                } else {
                    Pass::Rerank
                }
            }
            // The reranker once the meaning pass is shown.
            Pass::Meaning if self.shown >= Pass::Meaning && self.answered == self.asked => {
                Pass::Rerank
            }
            _ => return,
        };
        self.sent = next;
        let _ = self.queries.send(Query::Search {
            asked: self.asked,
            request: SearchRequest {
                typing: false,
                ..request.clone()
            },
            pass: next,
        });
    }

    #[cfg(test)]
    pub fn indexer(&self) -> &Indexer {
        &self.indexer
    }

    /// Asks for items close in meaning every so often while search by
    /// meaning runs, and takes the answer.
    fn poll_neighbours(&mut self) {
        if let Some(reply) = &self.neighbours_reply {
            match reply.try_recv() {
                Ok(found) => {
                    self.neighbours = Some(Arc::new(found));
                    self.neighbours_reply = None;
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => self.neighbours_reply = None,
            }
        }
        let due = self
            .neighbours_at
            .is_none_or(|at| at.elapsed() >= NEIGHBOURS_EVERY);
        if self.meaning && due {
            let (sender, reply) = mpsc::channel();
            if self.queries.send(Query::Neighbours(sender)).is_ok() {
                self.neighbours_reply = Some(reply);
                self.neighbours_at = Some(Instant::now());
            }
        }
    }

    /// Items close in meaning and topics, once worked out.
    pub fn neighbours(&self) -> Option<Arc<Neighbours>> {
        self.neighbours.clone()
    }

    /// A handle for the assistant's search.
    pub fn tool_searcher(&self) -> ToolSearcher {
        ToolSearcher(self.queries.clone())
    }

    /// The pass the shown results come from.
    #[cfg(test)]
    pub fn shown_pass(&self) -> Pass {
        self.shown
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

    /// Each item's text state, as of the index's last step.
    pub fn item_states(&self) -> glyphst_search::indexer::ItemStates {
        self.indexer.item_states()
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

    /// The searches and the passages opened from them, kept to measure
    /// search by (`--eval-search`).
    pub fn opened_path(&self) -> PathBuf {
        self.path.with_file_name("opened.jsonl")
    }

    /// Keeps a search and the passage opened from it, in the labelled
    /// set's form: the file and its page are what the search should find.
    pub fn record_opened(&self, query: &str, path: &Path, page: usize) -> Result<()> {
        use std::io::Write as _;
        let query = query.trim();
        let Some(name) = path.file_name() else {
            return Ok(());
        };
        if query.is_empty() {
            return Ok(());
        }
        let line = serde_json::json!({
            "q": query,
            "expect": [format!("{}@{}", name.to_string_lossy(), page + 1)],
            "tag": "opened",
            "at": glyphst_library::current_timestamp(),
        });
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.opened_path())?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Forgets them.
    pub fn forget_opened(&self) {
        let _ = std::fs::remove_file(self.opened_path());
    }

    /// The index's file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SearchService {
    fn drop(&mut self) {
        let _ = self.queries.send(Query::Stop);
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
            // A file another computer added that has not arrived yet.
            .filter(|attachment| attachment.path.exists())
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

    /// Three papers, one item each, indexed into `dir`.
    fn papers(dir: &Path, texts: &[&str]) -> Snapshot {
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
        snapshot
    }

    fn poll_until(service: &mut SearchService, ctx: &egui::Context, pass: Pass) {
        let started = Instant::now();
        while service.shown_pass() < pass || service.is_stale() {
            service.poll(ctx);
            assert!(
                started.elapsed().as_secs() < 30,
                "stuck at {:?}",
                service.shown_pass()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_search_answers_by_keyword_then_meaning_then_reranked() {
        use glyphst_search::embed::{FakeEmbedder, FakeReranker};
        let dir = std::env::temp_dir().join(format!("glyphst-passes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = egui::Context::default();
        let mut service = SearchService::start(&dir.join("search.sqlite"), &ctx).unwrap();
        service.indexer.reconcile(papers(
            &dir,
            &[
                "attention models attention models",
                "recurrent models of attention",
                "bread",
            ],
        ));
        service.set_embedder(Some(Arc::new(FakeEmbedder)));
        let started = Instant::now();
        while service.status().embedded < 3 {
            assert!(started.elapsed().as_secs() < 30, "{:?}", service.status());
            std::thread::sleep(Duration::from_millis(20));
        }
        let request = |text: &str| SearchRequest {
            text: text.to_string(),
            ..SearchRequest::default()
        };
        service.search(request("attention models"));
        poll_until(&mut service, &ctx, Pass::Meaning);
        let results = service.results().unwrap();
        assert!(results.meaning && !results.reranked);
        // Only meaning runs: the search stops there.
        service.poll(&ctx);
        assert_eq!(service.shown_pass(), Pass::Meaning);

        // The reranker joins: the search is reread.
        service.set_reranker(Some(Arc::new(FakeReranker)));
        poll_until(&mut service, &ctx, Pass::Rerank);
        assert!(service.results().unwrap().reranked);

        // A new search starts over at keywords, and goes all the way.
        service.search(request("recurrent"));
        poll_until(&mut service, &ctx, Pass::Rerank);
        let results = service.results().unwrap();
        assert!(results.reranked && results.meaning);
        assert_eq!(results.items[0].item_id, 2);
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn opened_passages_become_a_labelled_set_and_are_forgotten() {
        let dir = std::env::temp_dir().join(format!("glyphst-opened-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = egui::Context::default();
        let service = SearchService::start(&dir.join("search.sqlite"), &ctx).unwrap();
        let snapshot = papers(&dir, &["attention models", "recurrent networks", "bread"]);
        let recurrent = snapshot.files[1].path.clone();
        service.indexer.reconcile(snapshot);
        let started = Instant::now();
        while service.status().done < 3 {
            assert!(started.elapsed().as_secs() < 30, "{:?}", service.status());
            std::thread::sleep(Duration::from_millis(10));
        }
        service.record_opened("recurrent", &recurrent, 0).unwrap();
        service.record_opened("  ", &recurrent, 0).unwrap();
        let set = glyphst_search::eval::EvalSet::load(&service.opened_path()).unwrap();
        assert_eq!(set.queries.len(), 1, "an empty search is not kept");
        assert_eq!(set.queries[0].expect, ["1.pdf@1"]);
        let store = Store::open_reader(service.path()).unwrap();
        let report = glyphst_search::eval::compare(&store, &set, None, None).unwrap();
        assert_eq!(report.rows[0].scores.at1, 1.0, "{report:?}");
        service.forget_opened();
        assert!(!service.opened_path().exists());
        drop(service);
        let _ = std::fs::remove_dir_all(dir);
    }

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
                embedding: Default::default(),
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
            if service.shown_pass() == Pass::Meaning {
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
