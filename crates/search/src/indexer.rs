//! The thread that builds the index: it follows the library's snapshots
//! and reads documents in steps of a few pages, saving after each, so the
//! app can quit at any moment and the next run goes on from there.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;

use crate::{
    chunk::chunk_page,
    embed::Embedder,
    extract::{DocReader, format_name},
    index::{DocJob, IndexStatus, ItemTextState, Snapshot, Step},
    store::Store,
};

/// Pages read between saves.
pub const STEP_PAGES: usize = 30;
/// Characters carried from the previous page for the first anchor's prefix.
const TAIL_CHARS: usize = 24;
/// Passages embedded between saves.
const EMBED_BATCH: usize = 32;
/// How long embedding rests after the engine failed.
const EMBED_BACKOFF: Duration = Duration::from_secs(30);

enum Command {
    Reconcile(Snapshot),
    Reindex,
    /// Embed passages with this model, or stop embedding.
    Embedder(Option<Arc<dyn Embedder>>),
    Stop,
}

/// Each item's text state, as last published.
pub type ItemStates = Arc<std::collections::HashMap<u64, ItemTextState>>;

/// The indexing thread, stopped when dropped.
pub struct Indexer {
    commands: Sender<Command>,
    paused: Arc<AtomicBool>,
    status: Arc<Mutex<IndexStatus>>,
    items: Arc<Mutex<ItemStates>>,
    error: Arc<Mutex<Option<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl Indexer {
    /// Starts indexing into `path`; `notify` is called when the status
    /// changes (the app repaints).
    pub fn start(path: PathBuf, notify: impl Fn() + Send + 'static) -> Result<Self> {
        let store = Store::open(&path)?;
        let status = Arc::new(Mutex::new(store.status()?));
        let items = Arc::new(Mutex::new(Arc::new(
            store.item_text_states(None).unwrap_or_default(),
        )));
        let (commands, receiver) = mpsc::channel();
        let paused = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let (worker_paused, worker_status, worker_error, worker_items) =
            (paused.clone(), status.clone(), error.clone(), items.clone());
        // Built on its thread: an open document stays there.
        let handle = std::thread::Builder::new()
            .name("search-index".to_string())
            .spawn(move || {
                Worker {
                    store,
                    commands: receiver,
                    paused: worker_paused,
                    status: worker_status,
                    items: worker_items,
                    error: worker_error,
                    notify: Box::new(notify),
                    open: None,
                    embedder: None,
                    embed_rest_until: None,
                }
                .run()
            })?;
        Ok(Self {
            commands,
            paused,
            status,
            items,
            error,
            handle: Some(handle),
        })
    }

    /// Follows the library as it is now.
    pub fn reconcile(&self, snapshot: Snapshot) {
        let _ = self.commands.send(Command::Reconcile(snapshot));
    }

    pub fn reindex(&self) {
        let _ = self.commands.send(Command::Reindex);
    }

    /// Embeds every passage with `embedder` once the text is read (and
    /// drops other models' vectors); `None` stops.
    pub fn set_embedder(&self, embedder: Option<Arc<dyn Embedder>>) {
        let _ = self.commands.send(Command::Embedder(embedder));
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Release);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    pub fn status(&self) -> IndexStatus {
        self.status.lock().map(|status| *status).unwrap_or_default()
    }

    /// Each item's text state, as of the last step.
    pub fn item_states(&self) -> ItemStates {
        self.items
            .lock()
            .map(|items| items.clone())
            .unwrap_or_default()
    }

    /// The last error the index itself hit (not a document's).
    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|error| error.clone())
    }
}

impl Drop for Indexer {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The document being read, kept open between steps.
struct Open {
    doc_id: i64,
    reader: DocReader,
    /// The end of the text read so far, for the next anchor's prefix.
    tail: String,
    next_page: usize,
}

struct Worker {
    store: Store,
    commands: Receiver<Command>,
    paused: Arc<AtomicBool>,
    status: Arc<Mutex<IndexStatus>>,
    items: Arc<Mutex<ItemStates>>,
    error: Arc<Mutex<Option<String>>>,
    notify: Box<dyn Fn() + Send>,
    open: Option<Open>,
    embedder: Option<Arc<dyn Embedder>>,
    /// After the engine failed, embedding waits until then.
    embed_rest_until: Option<std::time::Instant>,
}

impl Worker {
    fn run(mut self) {
        loop {
            // Take what the app asked for, without waiting.
            loop {
                match self.commands.try_recv() {
                    Ok(command) => {
                        if !self.handle(command) {
                            return;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return,
                }
            }
            let job = if self.paused.load(Ordering::Acquire) {
                None
            } else {
                match self.store.next_doc(now()) {
                    Ok(job) => job,
                    Err(error) => {
                        self.report_error(format!("{error:#}"));
                        None
                    }
                }
            };
            match job {
                Some(job) => self.step(job),
                // Text first; then meaning, a batch at a time.
                None if !self.paused.load(Ordering::Acquire) && self.embed_step() => {}
                None => {
                    // Nothing to do: wait for the app, or the next retry.
                    let wait = self
                        .store
                        .next_retry()
                        .ok()
                        .flatten()
                        .map_or(60, |at| (at - now()).clamp(1, 60));
                    match self.commands.recv_timeout(Duration::from_secs(wait as u64)) {
                        Ok(command) => {
                            if !self.handle(command) {
                                return;
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
        }
    }

    /// Returns false to stop.
    fn handle(&mut self, command: Command) -> bool {
        let result = match command {
            Command::Stop => return false,
            Command::Reconcile(snapshot) => self.store.reconcile(&snapshot, now()),
            Command::Reindex => {
                self.open = None;
                self.store.reindex_all()
            }
            Command::Embedder(embedder) => {
                self.embed_rest_until = None;
                let result = match &embedder {
                    Some(embedder) => self
                        .store
                        .drop_other_vectors(embedder.model_key())
                        .map(drop),
                    None => Ok(()),
                };
                self.embedder = embedder;
                result
            }
        };
        if let Err(error) = result {
            self.report_error(format!("{error:#}"));
        }
        self.publish();
        true
    }

    /// Reads up to `STEP_PAGES` pages of the job's document and saves them.
    fn step(&mut self, job: DocJob) {
        if let Err(error) = self.try_step(&job) {
            self.open = None;
            log::warn!("could not index {}: {error:#}", job.path.display());
            if let Err(error) = self
                .store
                .fail_doc(job.doc_id, &format!("{error:#}"), now())
            {
                self.report_error(format!("{error:#}"));
            }
        }
        self.publish();
    }

    fn try_step(&mut self, job: &DocJob) -> Result<()> {
        let reopen = self
            .open
            .as_ref()
            .is_none_or(|open| open.doc_id != job.doc_id || open.next_page != job.pages_done);
        if reopen {
            let reader = DocReader::open(&job.path)?;
            // Resuming: the page before gives the first prefix.
            let tail = match job.pages_done.checked_sub(1) {
                Some(page) if page < reader.page_count => tail_of(
                    &reader
                        .page(page)?
                        .glyphs
                        .iter()
                        .map(|glyph| glyph.ch)
                        .collect::<String>(),
                ),
                _ => String::new(),
            };
            self.open = Some(Open {
                doc_id: job.doc_id,
                reader,
                tail,
                next_page: job.pages_done,
            });
        }
        let Some(open) = self.open.as_mut() else {
            return Ok(());
        };
        let page_count = open.reader.page_count;
        let end = (job.pages_done + STEP_PAGES).min(page_count);
        let started = std::time::Instant::now();
        let mut offset = job.glyphs_done;
        let mut pages = Vec::new();
        for page in job.pages_done..end {
            let text = open.reader.page(page)?;
            let chunks = chunk_page(&text, page, offset, &open.tail, open.reader.section(page));
            offset += text.glyphs.len();
            if !text.glyphs.is_empty() {
                open.tail = tail_of(&text.glyphs.iter().map(|glyph| glyph.ch).collect::<String>());
            }
            pages.push((page, chunks));
        }
        open.next_page = end;
        let read = end - job.pages_done;
        let ms = started.elapsed().as_secs_f64() * 1_000.0 / read.max(1) as f64;
        let format = format_name(open.reader.format);
        self.store.save_step(
            &Step {
                doc_id: job.doc_id,
                format,
                page_count,
                pages: &pages,
                pages_done: end,
                glyphs_done: offset,
            },
            now(),
        )?;
        self.store.record_estimate("page", ms, read)?;
        if end >= page_count {
            self.open = None;
        }
        Ok(())
    }

    fn publish(&self) {
        if let Ok(mut status) = self.store.status()
            && let Ok(mut shared) = self.status.lock()
        {
            if let Some(embedder) = &self.embedder {
                status.embedded = self.store.vector_count(embedder.model_key()).unwrap_or(0);
                status.meaning = true;
            }
            *shared = status;
        }
        let model = self.embedder.as_ref().map(|embedder| embedder.model_key());
        if let Ok(states) = self.store.item_text_states(model)
            && let Ok(mut shared) = self.items.lock()
        {
            *shared = Arc::new(states);
        }
        (self.notify)();
    }

    /// Embeds the next batch of passages; false when there were none (or
    /// no model, or the engine is resting after a failure).
    fn embed_step(&mut self) -> bool {
        let Some(embedder) = self.embedder.clone() else {
            return false;
        };
        if self
            .embed_rest_until
            .is_some_and(|until| std::time::Instant::now() < until)
        {
            return false;
        }
        let key = embedder.model_key().to_string();
        let pending = match self.store.next_to_embed(&key, EMBED_BATCH) {
            Ok(pending) if !pending.is_empty() => pending,
            Ok(_) => return false,
            Err(error) => {
                self.report_error(format!("{error:#}"));
                return false;
            }
        };
        let started = std::time::Instant::now();
        let passages = pending
            .iter()
            .map(|chunk| (chunk.title.as_str(), chunk.text.as_str()))
            .collect::<Vec<_>>();
        match embedder.embed_documents(&passages) {
            Ok(vectors) => {
                let rows = pending
                    .iter()
                    .zip(vectors)
                    .map(|(chunk, vector)| (chunk.chunk_id, chunk.title.clone(), vector))
                    .collect::<Vec<_>>();
                let ms = started.elapsed().as_secs_f64() * 1_000.0 / rows.len().max(1) as f64;
                if let Err(error) = self
                    .store
                    .save_vectors(&key, &rows)
                    .and_then(|()| self.store.record_estimate("passage", ms, rows.len()))
                {
                    self.report_error(format!("{error:#}"));
                }
                if let Ok(mut shared) = self.error.lock() {
                    *shared = None;
                }
            }
            Err(error) => {
                self.report_error(format!("meaning: {error:#}"));
                self.embed_rest_until = Some(std::time::Instant::now() + EMBED_BACKOFF);
            }
        }
        self.publish();
        true
    }

    fn report_error(&self, error: String) {
        log::warn!("search index: {error}");
        if let Ok(mut shared) = self.error.lock() {
            *shared = Some(error);
        }
    }
}

fn tail_of(text: &str) -> String {
    let count = text.chars().count();
    text.chars()
        .skip(count.saturating_sub(TAIL_CHARS))
        .collect()
}

pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

/// Runs up to `steps` steps on this thread, for tests.
#[cfg(test)]
pub(crate) fn run_steps(store: Store, steps: usize) -> Store {
    let (_commands, receiver) = mpsc::channel();
    let mut worker = Worker {
        store,
        commands: receiver,
        paused: Arc::new(AtomicBool::new(false)),
        status: Arc::new(Mutex::new(IndexStatus::default())),
        items: Arc::default(),
        error: Arc::new(Mutex::new(None)),
        notify: Box::new(|| {}),
        open: None,
        embedder: None,
        embed_rest_until: None,
    };
    for _ in 0..steps {
        match worker.store.next_doc(now()).unwrap() {
            Some(job) => worker.step(job),
            None => break,
        }
    }
    worker.store
}

/// Embeds up to `batches` batches on this thread, for tests.
#[cfg(test)]
pub(crate) fn run_embed(store: Store, embedder: Arc<dyn Embedder>, batches: usize) -> Store {
    let (_commands, receiver) = mpsc::channel();
    let mut worker = Worker {
        store,
        commands: receiver,
        paused: Arc::new(AtomicBool::new(false)),
        status: Arc::new(Mutex::new(IndexStatus::default())),
        items: Arc::default(),
        error: Arc::new(Mutex::new(None)),
        notify: Box::new(|| {}),
        open: None,
        embedder: Some(embedder),
        embed_rest_until: None,
    };
    for _ in 0..batches {
        if !worker.embed_step() {
            break;
        }
    }
    worker.store
}
