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
    extract::{DocReader, format_name},
    index::{DocJob, IndexStatus, Snapshot, Step},
    store::Store,
};

/// Pages read between saves.
pub const STEP_PAGES: usize = 30;
/// Characters carried from the previous page for the first anchor's prefix.
const TAIL_CHARS: usize = 24;

enum Command {
    Reconcile(Snapshot),
    Reindex,
    Stop,
}

/// The indexing thread, stopped when dropped.
pub struct Indexer {
    commands: Sender<Command>,
    paused: Arc<AtomicBool>,
    status: Arc<Mutex<IndexStatus>>,
    error: Arc<Mutex<Option<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl Indexer {
    /// Starts indexing into `path`; `notify` is called when the status
    /// changes (the app repaints).
    pub fn start(path: PathBuf, notify: impl Fn() + Send + 'static) -> Result<Self> {
        let store = Store::open(&path)?;
        let status = Arc::new(Mutex::new(store.status()?));
        let (commands, receiver) = mpsc::channel();
        let paused = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let (worker_paused, worker_status, worker_error) =
            (paused.clone(), status.clone(), error.clone());
        // Built on its thread: an open document stays there.
        let handle = std::thread::Builder::new()
            .name("search-index".to_string())
            .spawn(move || {
                Worker {
                    store,
                    commands: receiver,
                    paused: worker_paused,
                    status: worker_status,
                    error: worker_error,
                    notify: Box::new(notify),
                    open: None,
                }
                .run()
            })?;
        Ok(Self {
            commands,
            paused,
            status,
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

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Release);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    pub fn status(&self) -> IndexStatus {
        self.status.lock().map(|status| *status).unwrap_or_default()
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
    error: Arc<Mutex<Option<String>>>,
    notify: Box<dyn Fn() + Send>,
    open: Option<Open>,
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
        if let Ok(status) = self.store.status()
            && let Ok(mut shared) = self.status.lock()
        {
            *shared = status;
        }
        (self.notify)();
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
        error: Arc::new(Mutex::new(None)),
        notify: Box::new(|| {}),
        open: None,
    };
    for _ in 0..steps {
        match worker.store.next_doc(now()).unwrap() {
            Some(job) => worker.step(job),
            None => break,
        }
    }
    worker.store
}
