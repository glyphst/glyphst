//! Translation in the reader: the services shared by every tab, selection
//! translation cards, and document translation sessions whose pages (one-page
//! PDFs typeset by Typst) are shown in place of the originals as they arrive.
//!
//! The domain crates do the work (`translation` for text,
//! `pdf-translation-service` for documents); the session lifecycle follows
//! the GPUI Reader's `translation-ui`, the cards and panel pdfcraft's style.
//! Providers and keys are the user's settings, shared with the GPUI Reader.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use crate::reveal::RevealBlock;
use glyphst_pdf::{DocumentSnapshot, vector::PageRect};
use glyphst_pdf_translation_service::{
    CancellationToken, ExportLayout, PageFailureCode, PdfTranslationConfig,
    PdfTranslationExportRequest, PdfTranslationHoverRegion, PdfTranslationPageFailure,
    PdfTranslationPagePreview, PdfTranslationProgress, PdfTranslationService,
    PdfTranslationSessionRequest, PdfTranslationSessionResult, PdfTranslationSessionUpdate,
    TranslationScope,
};
use glyphst_settings::Settings;
use glyphst_translation::{
    LanguageCode, TranslationExecutionConfig, TranslationRequest, TranslationService,
};

/// Translations pinned to the Translation panel.
const MAX_PINNED: usize = 20;

/// Pages after the one being read that translate with it.
pub const FOLLOW_AHEAD: usize = 2;

/// How long the reader stays on a page before translation follows, so
/// paging or dragging through the document does not pull every page in.
const FOLLOW_DWELL: Duration = Duration::from_millis(350);

/// A selection translation shown as a card.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub id: u64,
    pub source: String,
    pub state: CardState,
    /// Screen position the card opens at.
    pub at: egui::Pos2,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CardState {
    Loading,
    Done(String),
    Failed(String),
}

/// The services and the selection translations of the app.
pub struct Translator {
    text: TranslationService,
    pdf: Arc<PdfTranslationService>,
    /// The user's settings, once read (see `UserSettings`).
    settings: Option<Arc<Settings>>,
    /// Problems with the translation cache.
    pub warnings: Vec<String>,
    results_tx: Sender<(u64, Result<String, String>)>,
    results_rx: Receiver<(u64, Result<String, String>)>,
    next_card: u64,
    pub card: Option<Card>,
    /// Pinned translations, newest first: source and translation.
    pub pinned: Vec<(String, String)>,
}

impl Translator {
    /// `persist` keeps translated text and translation jobs where the GPUI
    /// Reader keeps them; otherwise they stay in memory and a temporary
    /// directory. Typst typesets in a worker process of this executable
    /// (see `main`), or in this process for tests.
    pub fn new(persist: bool, typst_in_process: bool) -> Self {
        let text = TranslationService::new().expect("failed to create the translation client");
        let mut warnings = Vec::new();
        if persist && let Err(error) = text.initialize_persistent_cache() {
            warnings.push(format!("translation cache: {error:#}"));
        }
        let mut pdf = if typst_in_process {
            PdfTranslationService::new_in_process(text.clone())
        } else {
            PdfTranslationService::new(text.clone())
        };
        if !persist {
            pdf = pdf.with_artifact_root(scratch_artifacts());
        }
        let pdf = Arc::new(pdf);
        let (results_tx, results_rx) = mpsc::channel();
        Self {
            text,
            pdf,
            settings: None,
            warnings,
            results_tx,
            results_rx,
            next_card: 1,
            card: None,
            pinned: Vec::new(),
        }
    }

    /// The settings to translate with, as the user last edited them.
    pub fn set_settings(&mut self, settings: Option<&Arc<Settings>>) {
        self.settings = settings.cloned();
    }

    /// Takes finished translations.
    pub fn poll(&mut self) {
        while let Ok((id, result)) = self.results_rx.try_recv() {
            if let Some(card) = &mut self.card
                && card.id == id
            {
                card.state = match result {
                    Ok(text) => CardState::Done(text),
                    Err(error) => CardState::Failed(error),
                };
            }
        }
    }

    pub fn settings(&self) -> Option<&Arc<Settings>> {
        self.settings.as_ref()
    }

    /// The language to translate into: the reader's choice, else the
    /// settings'.
    pub fn target_language(&self, chosen: Option<&str>) -> Option<String> {
        chosen.map(str::to_string).or_else(|| {
            Some(
                self.settings
                    .as_ref()?
                    .translation
                    .resolved_target_language(),
            )
        })
    }

    /// Translates `text` into a card at `at`.
    pub fn translate_selection(
        &mut self,
        text: String,
        target: Option<&str>,
        at: egui::Pos2,
        ctx: &egui::Context,
    ) {
        let id = self.next_card;
        self.next_card += 1;
        let mut card = Card {
            id,
            source: text.clone(),
            state: CardState::Loading,
            at,
        };
        let request = self.settings.clone().ok_or_else(|| {
            "Your translation settings are still loading; try again in a moment.".to_string()
        });
        let request = request.and_then(|settings| {
            let target = self
                .target_language(target)
                .ok_or_else(|| "No target language is set.".to_string())?;
            let target_language = LanguageCode::new(&target).map_err(|error| error.to_string())?;
            let source_language = settings
                .translation
                .source_language
                .as_deref()
                .and_then(|language| LanguageCode::new(language).ok());
            Ok((
                TranslationExecutionConfig::from_settings(&settings),
                TranslationRequest {
                    provider: settings.translation.active_provider,
                    source_language,
                    target_language,
                    texts: vec![text],
                    context_before: None,
                    context_after: None,
                },
            ))
        });
        match request {
            Err(error) => card.state = CardState::Failed(error),
            Ok((config, request)) => {
                let (service, results, ctx) =
                    (self.text.clone(), self.results_tx.clone(), ctx.clone());
                thread::spawn(move || {
                    let result = service
                        .translate(&config, request)
                        .map(|response| {
                            response.translations.into_iter().next().unwrap_or_default()
                        })
                        .map_err(|error| format!("{error:#}"));
                    let _ = results.send((id, result));
                    ctx.request_repaint();
                });
            }
        }
        self.card = Some(card);
    }

    /// Keeps the card's translation in the Translation panel.
    pub fn pin_card(&mut self) {
        if let Some(Card {
            source,
            state: CardState::Done(translation),
            ..
        }) = &self.card
        {
            self.pinned.insert(0, (source.clone(), translation.clone()));
            self.pinned.truncate(MAX_PINNED);
        }
    }

    /// Starts translating a PDF into `target`, the pages `scope` covers.
    pub fn start_session(
        &self,
        path: &Path,
        target: Option<&str>,
        scope: Arc<TranslationScope>,
        options: SessionOptions,
        ctx: &egui::Context,
    ) -> Result<DocSession, String> {
        let settings = self
            .settings
            .clone()
            .ok_or("Your translation settings are still loading; try again in a moment.")?;
        let target = self
            .target_language(target)
            .ok_or("No target language is set.")?;
        let target_language = LanguageCode::new(&target).map_err(|error| error.to_string())?;
        let snapshot =
            Arc::new(DocumentSnapshot::read(path).map_err(|error| format!("{error:#}"))?);
        let request = PdfTranslationSessionRequest {
            source_path: path.to_path_buf(),
            source_language: settings
                .translation
                .source_language
                .as_deref()
                .and_then(|language| LanguageCode::new(language).ok()),
            target_language,
        };
        let mut config = PdfTranslationConfig::from_settings(&settings);
        // A retry recognises again the pages that failed last time.
        if options.retry {
            config = config.retrying_failed_pages();
        }
        if let Some(ocr) = &options.ocr {
            config = config.with_document_ocr(&ocr.engine, ocr.mode.as_deref());
        }
        let cancel = CancellationToken::new();
        let (events_tx, events) = mpsc::channel();
        let (service, worker_cancel, ctx) = (self.pdf.clone(), cancel.clone(), ctx.clone());
        let worker_scope = scope.clone();
        let spawned = thread::Builder::new()
            .name("pdf-translation".into())
            .spawn(move || {
                let progress_tx = events_tx.clone();
                let progress_ctx = ctx.clone();
                let update_tx = events_tx.clone();
                let update_ctx = ctx.clone();
                let result = service.run_scoped_session_from_snapshot(
                    config,
                    request,
                    snapshot,
                    &worker_scope,
                    worker_cancel,
                    move |progress| {
                        let _ = progress_tx.send(SessionEvent::Progress(progress));
                        progress_ctx.request_repaint();
                    },
                    move |update| {
                        let _ = update_tx.send(SessionEvent::Update(Box::new(update)));
                        update_ctx.request_repaint();
                    },
                );
                let _ = events_tx.send(SessionEvent::Done(Box::new(
                    result.map_err(|error| error.message),
                )));
                ctx.request_repaint();
            });
        spawned.map_err(|error| format!("could not start the translation: {error}"))?;
        Ok(DocSession {
            target,
            status: SessionStatus::Running,
            progress: None,
            page_count: 0,
            ready_pages: 0,
            translatable: BTreeMap::new(),
            settled: HashSet::new(),
            previews: HashMap::new(),
            failures: Vec::new(),
            result: None,
            show_translated: true,
            cancel,
            events,
            export: None,
            export_layout: ExportLayout::default(),
            reveals: Vec::new(),
            scope,
            dwell: None,
        })
    }

    /// Writes a finished session's PDF to `output`: the translated pages,
    /// or each original beside its translation.
    pub fn export(
        &self,
        session: &mut DocSession,
        output: PathBuf,
        layout: ExportLayout,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let settings = self.settings.clone().ok_or("settings are not loaded")?;
        let result = session
            .result
            .clone()
            .ok_or("The translation is not finished yet.")?;
        let (service, ctx) = (self.pdf.clone(), ctx.clone());
        let (done_tx, done) = mpsc::channel();
        thread::spawn(move || {
            let exported = service
                .export_session(
                    PdfTranslationConfig::from_settings(&settings),
                    result,
                    PdfTranslationExportRequest {
                        output_path: output,
                        original_unit_ids: Vec::new(),
                        layout,
                    },
                    CancellationToken::new(),
                    |_| {},
                )
                .map(|result| result.output_path)
                .map_err(|error| error.message);
            let _ = done_tx.send(exported);
            ctx.request_repaint();
        });
        session.export = Some(Export::Running(done));
        Ok(())
    }
}

/// Where runs that save nothing keep translation jobs.
pub fn scratch_artifacts() -> PathBuf {
    std::env::temp_dir().join(format!(
        "glyphst-reader-translations-{}",
        std::process::id()
    ))
}

enum SessionEvent {
    Progress(PdfTranslationProgress),
    Update(Box<PdfTranslationSessionUpdate>),
    Done(Box<Result<PdfTranslationSessionResult, String>>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum SessionStatus {
    Running,
    Finished,
    Failed(String),
}

/// How a session starts: again over pages that failed, and with the
/// document's own OCR choice.
#[derive(Clone, Debug, Default)]
pub struct SessionOptions {
    pub retry: bool,
    pub ocr: Option<glyphst_library::DocumentOcr>,
}

/// Blocks a page's new preview shows translated, to reveal.
pub struct RevealArrival {
    pub page: usize,
    pub revision: u64,
    pub blocks: Vec<RevealBlock>,
    /// The page without its translation, shown under unresolved
    /// characters.
    pub base: Option<Arc<[u8]>>,
}

/// A translated page: its typeset PDF and where its blocks came from.
#[derive(Clone, Debug)]
pub struct Preview {
    pub revision: u64,
    pub pdf: Arc<[u8]>,
    pub hover: Vec<PdfTranslationHoverRegion>,
    /// Why each translated block that broke the page keeps its original.
    pub kept_original: Vec<String>,
}

/// How far a page's translation has come, for the Translation panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageProgress {
    /// Every block to translate is translated.
    Done,
    /// Some blocks are in; `done` of `total`.
    Partial { done: usize, total: usize },
    /// It could not be translated, and why.
    Failed(PageFailureCode),
    /// In scope and on its way.
    Waiting,
    /// Out of scope, or the session ended without it.
    NotTranslated,
}

pub enum Export {
    Running(Receiver<Result<PathBuf, String>>),
    Done(Result<PathBuf, String>),
}

/// The translation of one document.
pub struct DocSession {
    pub target: String,
    pub status: SessionStatus,
    pub progress: Option<PdfTranslationProgress>,
    pub page_count: usize,
    /// Pages ready from the start of the document.
    pub ready_pages: usize,
    /// The blocks to translate on each prepared page, with their boxes.
    pub translatable: BTreeMap<usize, Vec<(String, PageRect)>>,
    /// Blocks whose translation is in, or has failed.
    settled: HashSet<String>,
    pub previews: HashMap<usize, Preview>,
    pub failures: Vec<PdfTranslationPageFailure>,
    pub result: Option<PdfTranslationSessionResult>,
    /// Translated pages are shown in place of the originals.
    pub show_translated: bool,
    cancel: CancellationToken,
    events: Receiver<SessionEvent>,
    pub export: Option<Export>,
    /// What the next export writes.
    pub export_layout: ExportLayout,
    /// Blocks newly translated by a page's revision, for the page to
    /// reveal (see `take_reveals`).
    reveals: Vec<RevealArrival>,
    /// The pages the session may translate; it follows the reader until
    /// it is made whole.
    scope: Arc<TranslationScope>,
    /// A page the reader moved to and since when, before the scope follows.
    dwell: Option<(usize, Instant)>,
}

impl DocSession {
    /// Applies the session's progress; returns whether translated pages
    /// changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            match event {
                SessionEvent::Progress(progress) => self.progress = Some(progress),
                SessionEvent::Update(update) => match *update {
                    PdfTranslationSessionUpdate::PagePrepared {
                        page,
                        page_count,
                        ready_prefix,
                        ..
                    } => {
                        self.page_count = page_count;
                        self.ready_pages = self.ready_pages.max(ready_prefix);
                        let blocks = page
                            .blocks
                            .iter()
                            .filter(|block| {
                                block.policy.translate && !block.content.text.trim().is_empty()
                            })
                            .map(|block| {
                                let b = block.bbox;
                                (block.id.clone(), [b.x0, b.y0, b.x1, b.y1])
                            })
                            .collect();
                        self.translatable.insert(page.page_index, blocks);
                    }
                    PdfTranslationSessionUpdate::PagePreviewReady(preview) => {
                        let newer = self
                            .previews
                            .get(&preview.page_index)
                            .is_none_or(|current| current.revision < preview.revision);
                        if newer {
                            let blocks =
                                reveal_blocks(self.previews.get(&preview.page_index), &preview);
                            if !blocks.is_empty() {
                                self.reveals.push(RevealArrival {
                                    page: preview.page_index,
                                    revision: preview.revision,
                                    blocks,
                                    base: preview.base_pdf.clone(),
                                });
                            }
                            self.previews.insert(
                                preview.page_index,
                                Preview {
                                    revision: preview.revision,
                                    pdf: preview.pdf.clone(),
                                    hover: preview.hover_regions.clone(),
                                    kept_original: preview
                                        .layout_failures
                                        .iter()
                                        .map(|failure| failure.reason.clone())
                                        .collect(),
                                },
                            );
                            changed = true;
                        }
                    }
                    PdfTranslationSessionUpdate::PageFailed(failure) => {
                        self.failures
                            .retain(|known| known.page_index != failure.page_index);
                        self.failures.push(failure);
                    }
                    PdfTranslationSessionUpdate::Reconciled { ready_prefix, .. } => {
                        self.ready_pages = self.ready_pages.max(ready_prefix);
                    }
                    PdfTranslationSessionUpdate::UnitsChanged(snapshot) => {
                        self.settled = snapshot
                            .resolved_units
                            .iter()
                            .flat_map(|unit| unit.member_block_ids.iter())
                            .chain(
                                // A failed block shows the original; one being
                                // retried may come in later.
                                snapshot
                                    .failures
                                    .iter()
                                    .flat_map(|failure| failure.member_block_ids.iter()),
                            )
                            .cloned()
                            .collect();
                    }
                    PdfTranslationSessionUpdate::CacheReady(_)
                    | PdfTranslationSessionUpdate::Finished => {}
                },
                SessionEvent::Done(result) => match *result {
                    Ok(result) => {
                        self.result = Some(result);
                        self.status = SessionStatus::Finished;
                    }
                    Err(error) => self.status = SessionStatus::Failed(error),
                },
            }
        }
        if let Some(Export::Running(done)) = &self.export
            && let Ok(result) = done.try_recv()
        {
            self.export = Some(Export::Done(result));
        }
        changed
    }

    pub fn is_running(&self) -> bool {
        self.status == SessionStatus::Running
    }

    /// Whether the session is still working on what the reader asked for:
    /// a session following the reader rests once its window is in.
    pub fn is_busy(&self) -> bool {
        match self.window() {
            _ if !self.is_running() => false,
            Some((first, last)) => {
                self.page_count == 0
                    || (first..=last).any(|page| {
                        // A page that failed is as far as it will get.
                        !matches!(
                            self.page_progress(page),
                            PageProgress::Done | PageProgress::Failed(_)
                        )
                    })
            }
            None => true,
        }
    }

    /// The newly translated blocks of each page's latest revisions: the
    /// page, the revision showing them, and the blocks.
    pub fn take_reveals(&mut self) -> Vec<RevealArrival> {
        std::mem::take(&mut self.reveals)
    }

    /// Boxes of the blocks on `page` still waiting for their translation,
    /// while the session is on its way to them.
    pub fn waiting_blocks(&self, page: usize) -> Vec<PageRect> {
        if !self.is_running() || self.scope.view().rank(page).is_none() {
            return Vec::new();
        }
        self.translatable
            .get(&page)
            .into_iter()
            .flatten()
            .filter(|(block, _)| !self.settled.contains(block))
            .map(|(_, area)| *area)
            .collect()
    }

    /// Whether the session translates only around the page being read.
    pub fn is_following(&self) -> bool {
        !self.scope.is_whole()
    }

    /// The first and last page of the reader's window while following.
    pub fn window(&self) -> Option<(usize, usize)> {
        let (focus, last) = self.scope.window();
        let last = match self.page_count {
            0 => last,
            count => last.min(count - 1).max(focus),
        };
        self.is_following().then_some((focus, last))
    }

    /// Tells a following session the reader is on `page`; once they have
    /// stayed there a moment the translation moves with them. Returns when
    /// to look again, while the reader's stay is still too short.
    pub fn follow(&mut self, page: usize, now: Instant) -> Option<Duration> {
        if !self.is_running() || !self.is_following() || self.scope.focus() == page {
            self.dwell = None;
            return None;
        }
        let since = match self.dwell {
            Some((dwelt, since)) if dwelt == page => since,
            _ => {
                self.dwell = Some((page, now));
                now
            }
        };
        let stayed = now.saturating_duration_since(since);
        if stayed >= FOLLOW_DWELL {
            self.scope.set_focus(page);
            self.dwell = None;
            None
        } else {
            Some(FOLLOW_DWELL - stayed)
        }
    }

    /// Translates every page from now on, starting from the reader's.
    pub fn translate_rest(&self) {
        self.scope.expand_to_whole();
    }

    /// Pages left untranslated: out of scope, or not yet in.
    pub fn untranslated_pages(&self) -> usize {
        let total = self.page_count;
        total.saturating_sub(
            (0..total)
                .filter(|page| self.page_progress(*page) == PageProgress::Done)
                .count(),
        )
    }

    /// Shows `previews` (another session's) until this session's own
    /// replace them.
    pub fn keep_previews(&mut self, previews: HashMap<usize, Preview>) {
        for (page, mut preview) in previews {
            // Any revision of this session's is newer.
            preview.revision = 0;
            self.previews.entry(page).or_insert(preview);
        }
    }

    /// Records that a page could not be translated (screens give some).
    pub fn add_failure(&mut self, failure: PdfTranslationPageFailure) {
        self.failures
            .retain(|known| known.page_index != failure.page_index);
        self.failures.push(failure);
    }

    /// Why `page` could not be translated, if it could not.
    pub fn failure(&self, page: usize) -> Option<&PdfTranslationPageFailure> {
        self.failures
            .iter()
            .find(|failure| failure.page_index == page)
    }

    /// The pages that could not be translated, in order.
    pub fn failed_pages(&self) -> Vec<usize> {
        let mut pages: Vec<usize> = self
            .failures
            .iter()
            .map(|failure| failure.page_index)
            .collect();
        pages.sort_unstable();
        pages.dedup();
        pages
    }

    /// Whether a page failed for want of a local OCR model.
    pub fn needs_local_ocr(&self) -> bool {
        self.failures
            .iter()
            .any(|failure| failure.code == PageFailureCode::LocalOcrMissing)
    }

    /// How far `page` has come.
    pub fn page_progress(&self, page: usize) -> PageProgress {
        if let Some(failure) = self.failure(page) {
            return PageProgress::Failed(failure.code);
        }
        let blocks = self.translatable.get(&page);
        let total = blocks.map_or(0, Vec::len);
        let done = blocks.map_or(0, |blocks| {
            blocks
                .iter()
                .filter(|(block, _)| self.settled.contains(block))
                .count()
        });
        let previewed = self.previews.contains_key(&page);
        // Once the session ends, what it shows is what there is.
        if (blocks.is_some() && done == total && (previewed || total == 0))
            || (!self.is_running() && previewed)
        {
            return PageProgress::Done;
        }
        if done > 0 || previewed {
            return PageProgress::Partial { done, total };
        }
        if self.is_running() && self.scope.view().rank(page).is_some() {
            PageProgress::Waiting
        } else {
            PageProgress::NotTranslated
        }
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// The translated page shown for `page`, if any.
    pub fn shown_preview(&self, page: usize) -> Option<&Preview> {
        self.show_translated
            .then(|| self.previews.get(&page))
            .flatten()
    }

    /// A line describing the progress.
    pub fn status_line(&self) -> String {
        match &self.status {
            SessionStatus::Finished => format!("Translated {} pages", self.previews.len()),
            SessionStatus::Failed(error) => format!("Stopped: {error}"),
            SessionStatus::Running => {
                let line = match &self.progress {
                    Some(progress) => {
                        let count = match (progress.completed, progress.total) {
                            (Some(done), Some(total)) => format!(" {done}/{total}"),
                            _ => String::new(),
                        };
                        format!("{}{count}", stage_label(progress))
                    }
                    None => "Starting…".into(),
                };
                match self.window() {
                    Some((first, last)) => format!("{line} · p.{}–{}", first + 1, last + 1),
                    None => line,
                }
            }
        }
    }
}

impl Drop for DocSession {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// The blocks `preview` shows translated that `previous` did not, with the
/// translated glyphs inside each, top to bottom.
fn reveal_blocks(
    previous: Option<&Preview>,
    preview: &PdfTranslationPagePreview,
) -> Vec<RevealBlock> {
    let before: HashSet<&str> = previous
        .into_iter()
        .flat_map(|previous| &previous.hover)
        .filter(|region| region.translated)
        .map(|region| region.id.as_str())
        .collect();
    let lines: Vec<Vec<PageRect>> = preview
        .text_layer
        .blocks
        .iter()
        .flat_map(|block| &block.lines)
        .map(|line| {
            line.glyphs
                .iter()
                .filter(|glyph| !glyph.character.is_whitespace())
                .map(|glyph| {
                    let q = &glyph.quad;
                    let xs = [
                        q.upper_left.x,
                        q.upper_right.x,
                        q.lower_left.x,
                        q.lower_right.x,
                    ];
                    let ys = [
                        q.upper_left.y,
                        q.upper_right.y,
                        q.lower_left.y,
                        q.lower_right.y,
                    ];
                    [
                        xs.into_iter().fold(f32::INFINITY, f32::min),
                        ys.into_iter().fold(f32::INFINITY, f32::min),
                        xs.into_iter().fold(f32::NEG_INFINITY, f32::max),
                        ys.into_iter().fold(f32::NEG_INFINITY, f32::max),
                    ]
                })
                .collect()
        })
        .collect();
    let union = |a: PageRect, b: PageRect| {
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ]
    };
    // A region's whole area: its translation, the box Typst set it in
    // (text can run past the translation's measured bounds) and its
    // original.
    let area_of = |region: &PdfTranslationHoverRegion| {
        let t = region.target_bounds;
        let mut area = [t.x0, t.y0, t.x1, t.y1];
        for item in preview
            .layout
            .items
            .iter()
            .filter(|item| region.block_id.as_deref() == Some(item.block_id.as_str()))
        {
            let b = item.bbox;
            area = union(area, [b.x0, b.y0, b.x1, b.y1]);
        }
        region
            .source_regions
            .iter()
            .fold(area, |area, b| union(area, [b.x0, b.y0, b.x1, b.y1]))
    };
    let (new, old): (Vec<_>, Vec<_>) = preview
        .hover_regions
        .iter()
        .partition(|region| region.translated && !before.contains(region.id.as_str()));
    let old: Vec<PageRect> = old.into_iter().map(area_of).collect();
    // Where each new block's original was: its translation shows it there
    // until the block is revealed.
    let originals: Vec<PageRect> = new
        .iter()
        .map(|region| {
            let t = region.target_bounds;
            let mut boxes = region
                .source_regions
                .iter()
                .map(|b| [b.x0, b.y0, b.x1, b.y1]);
            let first = boxes.next().unwrap_or([t.x0, t.y0, t.x1, t.y1]);
            boxes.fold(first, union)
        })
        .collect();
    let areas: Vec<PageRect> = new.into_iter().map(area_of).collect();
    let centre = |g: &PageRect| ((g[0] + g[2]) / 2.0, (g[1] + g[3]) / 2.0);
    let contains = |a: &PageRect, (x, y): (f32, f32), margin: f32| {
        x >= a[0] - margin && x <= a[2] + margin && y >= a[1] - margin && y <= a[3] + margin
    };
    let distance = |a: &PageRect, (x, y): (f32, f32)| {
        let dx = (a[0] - x).max(x - a[2]).max(0.0);
        let dy = (a[1] - y).max(y - a[3]).max(0.0);
        dx.hypot(dy)
    };
    let mut heights: Vec<f32> = lines.iter().flatten().map(|g| g[3] - g[1]).collect();
    heights.sort_by(f32::total_cmp);
    let reach = heights.get(heights.len() / 2).copied().unwrap_or(10.0) * 1.5;
    // Every glyph goes to the new block holding it, or else the nearest one
    // within reach unless an older block holds it: a translated glyph in no
    // block would show before its block is revealed.
    let mut glyphs: Vec<Vec<(usize, PageRect)>> = vec![Vec::new(); areas.len()];
    for (line, glyph) in lines
        .iter()
        .enumerate()
        .flat_map(|(line, glyphs)| glyphs.iter().map(move |glyph| (line, *glyph)))
    {
        let point = centre(&glyph);
        let owner = areas
            .iter()
            .position(|area| contains(area, point, 1.0))
            .or_else(|| {
                if old.iter().any(|area| contains(area, point, 1.0)) {
                    return None;
                }
                areas
                    .iter()
                    .enumerate()
                    .map(|(index, area)| (index, distance(area, point)))
                    .filter(|(_, distance)| *distance <= reach)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(index, _)| index)
            });
        if let Some(owner) = owner {
            glyphs[owner].push((line, glyph));
        }
    }
    let mut blocks: Vec<RevealBlock> = areas
        .into_iter()
        .zip(originals)
        .zip(glyphs)
        .map(|((area, original), glyphs)| {
            // The block's glyphs by text line, in reading order.
            let mut by_line: Vec<(usize, Vec<PageRect>)> = Vec::new();
            for (line, glyph) in glyphs {
                match by_line.last_mut() {
                    Some((last, glyphs)) if *last == line => glyphs.push(glyph),
                    _ => by_line.push((line, vec![glyph])),
                }
            }
            let lines = by_line.into_iter().map(|(_, glyphs)| glyphs).collect();
            RevealBlock::from_glyphs(area, original, lines)
        })
        .collect();
    blocks.sort_by(|a, b| a.area[1].total_cmp(&b.area[1]));
    blocks
}

fn stage_label(progress: &PdfTranslationProgress) -> &'static str {
    use glyphst_pdf_translation_service::PdfTranslationStage as Stage;
    match progress.stage {
        Stage::Preparing => "Preparing",
        Stage::Ocr => "Recognizing text",
        Stage::Normalizing => "Reading the layout",
        Stage::Translating => "Translating",
        Stage::Layout => "Laying out",
        Stage::Compiling => "Typesetting",
        Stage::Composing => "Composing",
        Stage::Validating => "Checking",
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use glyphst_pdf_translation_service::{
        PdfTranslationPageGlyph, PdfTranslationPageQuad, PdfTranslationPageTextBlock,
        PdfTranslationPageTextLayer, PdfTranslationPageTextLine,
    };

    use super::*;
    use crate::fake_providers::{fake_translation, start_translation_server, translation_settings};

    fn wait_until(mut done: impl FnMut() -> bool) {
        let started = Instant::now();
        while !done() {
            assert!(started.elapsed() < Duration::from_secs(180), "timed out");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn translator(_ctx: &egui::Context) -> Translator {
        let port = start_translation_server().unwrap();
        let mut translator = Translator::new(false, true);
        translator.set_settings(Some(&Arc::new(translation_settings(port))));
        translator
    }

    fn glyph(character: char, [x0, y0, x1, y1]: PageRect) -> PdfTranslationPageGlyph {
        use glyphst_pdf_translation_service::PdfTranslationPagePoint as Point;
        PdfTranslationPageGlyph {
            character,
            quad: PdfTranslationPageQuad {
                upper_left: Point::new(x0, y0),
                upper_right: Point::new(x1, y0),
                lower_left: Point::new(x0, y1),
                lower_right: Point::new(x1, y1),
            },
            font_size: 10.0,
            bold: false,
        }
    }

    fn region(id: &str, translated: bool, [x0, y0, x1, y1]: PageRect) -> PdfTranslationHoverRegion {
        let bounds = glyphst_pdf_translation_service::BoundingBox::new(x0, y0, x1, y1);
        PdfTranslationHoverRegion {
            id: id.to_string(),
            kind: glyphst_pdf_translation_service::PdfTranslationHoverRegionKind::Text,
            translated,
            unit_id: None,
            block_id: None,
            target_bounds: bounds,
            source_regions: vec![bounds],
        }
    }

    #[test]
    fn a_translated_glyph_set_past_its_block_is_still_revealed_with_it() {
        let line = |glyphs: Vec<PdfTranslationPageGlyph>| PdfTranslationPageTextLine {
            bounds: glyphst_pdf_translation_service::BoundingBox::new(0.0, 0.0, 0.0, 0.0),
            glyphs,
        };
        let preview = PdfTranslationPagePreview {
            page_index: 0,
            revision: 1,
            layout: glyphst_pdf_translation_service::LayoutPage {
                page_index: 0,
                geometry: glyphst_pdf_translation_service::PageGeometry::new(200.0, 300.0),
                items: Vec::new(),
            },
            hover_regions: vec![
                region("new", true, [10.0, 10.0, 100.0, 30.0]),
                region("kept", false, [10.0, 200.0, 100.0, 220.0]),
            ],
            text_layer: PdfTranslationPageTextLayer {
                blocks: vec![PdfTranslationPageTextBlock {
                    lines: vec![
                        line(vec![glyph('a', [12.0, 12.0, 18.0, 22.0])]),
                        // Typst ran a line past the measured translation.
                        line(vec![glyph('b', [12.0, 33.0, 18.0, 43.0])]),
                        line(vec![glyph('c', [12.0, 205.0, 18.0, 215.0])]),
                    ],
                }],
            },
            pdf: Arc::from(Vec::new()),
            base_pdf: None,
            layout_failures: Vec::new(),
        };
        let blocks = reveal_blocks(None, &preview);
        assert_eq!(blocks.len(), 1);
        let glyphs: usize = blocks[0].lines.iter().map(|line| line.glyphs.len()).sum();
        assert_eq!(glyphs, 2, "{blocks:?}");
        assert_eq!(blocks[0].lines.len(), 2);
        assert!(blocks[0].area[3] >= 43.0, "{:?}", blocks[0].area);
    }

    #[test]
    fn selections_translate_through_the_provider_and_problems_show_in_the_card() {
        let ctx = egui::Context::default();
        let mut translator = translator(&ctx);
        translator.translate_selection("Hello world".into(), None, egui::Pos2::ZERO, &ctx);
        wait_until(|| {
            translator.poll();
            translator.card.as_ref().unwrap().state != CardState::Loading
        });
        assert_eq!(
            translator.card.as_ref().unwrap().state,
            CardState::Done(fake_translation("Hello world"))
        );
        translator.pin_card();
        assert_eq!(translator.pinned[0].1, fake_translation("Hello world"));

        translator.translate_selection("x".into(), Some("not a language!"), egui::Pos2::ZERO, &ctx);
        assert!(matches!(
            translator.card.as_ref().unwrap().state,
            CardState::Failed(_)
        ));
        translator.settings = None;
        translator.translate_selection("x".into(), None, egui::Pos2::ZERO, &ctx);
        let CardState::Failed(message) = &translator.card.as_ref().unwrap().state else {
            panic!("a card without settings fails");
        };
        assert!(message.contains("loading"));
    }

    /// By hand, on a real document: `GLYPHST_TEST_PDF` (its path) and
    /// `GLYPHST_TEST_PAGE` (from 1), then
    /// `cargo test -p glyphst-reader a_real_page -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a PDF named by GLYPHST_TEST_PDF"]
    fn a_real_page_translates_without_failing() {
        let Some(path) = std::env::var_os("GLYPHST_TEST_PDF").map(std::path::PathBuf::from) else {
            eprintln!("GLYPHST_TEST_PDF is not set");
            return;
        };
        let page = std::env::var("GLYPHST_TEST_PAGE")
            .ok()
            .and_then(|page| page.parse::<usize>().ok())
            .unwrap_or(1)
            .saturating_sub(1);
        let ctx = egui::Context::default();
        let translator = translator(&ctx);
        let mut session = translator
            .start_session(
                &path,
                None,
                TranslationScope::following(page, 0),
                SessionOptions::default(),
                &ctx,
            )
            .unwrap();
        wait_until(|| {
            session.poll();
            session.failure(page).is_some()
                || session.page_progress(page) == PageProgress::Done
                || !session.is_running()
        });
        assert_eq!(session.failure(page), None);
        let preview = session.previews.get(&page).expect("the page has a preview");
        for reason in &preview.kept_original {
            eprintln!("kept original: {reason}");
        }
    }

    #[test]
    fn a_following_session_translates_its_window_then_the_rest() {
        let directory = scratch_artifacts().join("following");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("source.pdf");
        let pages: Vec<String> = (0..6)
            .map(|page| format!("Page {page} says hello."))
            .collect();
        let pages: Vec<[&str; 1]> = pages.iter().map(|text| [text.as_str()]).collect();
        let pages: Vec<&[&str]> = pages.iter().map(|page| page.as_slice()).collect();
        std::fs::write(&path, glyphst_pdf::test_pdf::text_pages_pdf(&pages)).unwrap();
        let ctx = egui::Context::default();
        let translator = translator(&ctx);
        let mut session = translator
            .start_session(
                &path,
                None,
                TranslationScope::following(1, 1),
                SessionOptions::default(),
                &ctx,
            )
            .unwrap();
        wait_until(|| {
            session.poll();
            session.previews.contains_key(&1) && session.previews.contains_key(&2)
        });
        thread::sleep(Duration::from_millis(300));
        session.poll();
        assert!(
            session.is_running(),
            "a following session waits for the reader"
        );
        assert_eq!(session.window(), Some((1, 2)));
        assert_eq!(session.page_progress(1), PageProgress::Done);
        assert_eq!(session.page_progress(4), PageProgress::NotTranslated);
        let mut shown: Vec<usize> = session.previews.keys().copied().collect();
        shown.sort_unstable();
        assert_eq!(shown, vec![1, 2], "only the window is translated");

        // The reader stays on page 4 long enough: it follows.
        let start = Instant::now();
        assert!(
            session.follow(4, start).is_some(),
            "a short stay is not enough"
        );
        assert_eq!(session.follow(4, start + FOLLOW_DWELL), None);
        assert_eq!(session.window(), Some((4, 5)));
        wait_until(|| {
            session.poll();
            session.previews.contains_key(&4) && session.previews.contains_key(&5)
        });
        assert_eq!(
            session.untranslated_pages(),
            2,
            "the first and fourth pages are left"
        );

        session.translate_rest();
        wait_until(|| {
            session.poll();
            !session.is_running()
        });
        assert_eq!(
            session.status,
            SessionStatus::Finished,
            "{}",
            session.status_line()
        );
        assert_eq!(session.untranslated_pages(), 0);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn documents_translate_into_typeset_pages() {
        // Its own folder: the artifacts beside it serve the other tests' sessions.
        let directory = scratch_artifacts().join("typeset");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("source.pdf");
        std::fs::write(
            &path,
            glyphst_pdf::test_pdf::text_pdf(&["A short document.", "It has two lines."]),
        )
        .unwrap();
        let ctx = egui::Context::default();
        let translator = translator(&ctx);
        let mut session = translator
            .start_session(
                &path,
                None,
                TranslationScope::whole(),
                SessionOptions::default(),
                &ctx,
            )
            .unwrap();
        assert_eq!(session.target, "fr");
        wait_until(|| {
            session.poll();
            !session.is_running()
        });
        assert_eq!(
            session.status,
            SessionStatus::Finished,
            "{}",
            session.status_line()
        );
        let preview = session.shown_preview(0).expect("page 1 is translated");
        let document = mupdf::Document::from_bytes(&preview.pdf, "application/pdf").unwrap();
        let text = glyphst_pdf::text::PageText::from_page(&document.load_page(0).unwrap())
            .unwrap()
            .plain_text();
        assert!(
            text.contains('«'),
            "the translated page shows the translation: {text}"
        );
        // Its base is the page with neither the translation nor the
        // original text it replaces.
        let arrival = session
            .take_reveals()
            .into_iter()
            .rfind(|arrival| arrival.page == 0)
            .expect("page 1's blocks are revealed");
        let base = arrival
            .base
            .as_ref()
            .expect("reveals carry the page's base");
        let base = mupdf::Document::from_bytes(base, "application/pdf").unwrap();
        assert_eq!(base.page_count().unwrap(), 1);
        let base_text = glyphst_pdf::text::PageText::from_page(&base.load_page(0).unwrap())
            .unwrap()
            .plain_text();
        assert!(
            !base_text.contains('«') && !base_text.contains("short document"),
            "the base is blank where the translation goes: {base_text}"
        );
        session.show_translated = false;
        assert!(session.shown_preview(0).is_none(), "the original is shown");
        let _ = std::fs::remove_dir_all(&directory);
    }
}
