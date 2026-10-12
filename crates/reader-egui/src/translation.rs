//! Translation in the reader: the services shared by every tab, selection
//! translation cards, and document translation sessions whose pages (one-page
//! PDFs typeset by Typst) are shown in place of the originals as they arrive.
//!
//! The domain crates do the work (`translation` for text,
//! `pdf-translation-service` for documents); the session lifecycle follows
//! the GPUI Reader's `translation-ui`, the cards and panel pdfcraft's style.
//! Providers and keys are the user's settings, shared with the GPUI Reader.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

use glyphst_pdf::DocumentSnapshot;
use glyphst_pdf_translation_service::{
    CancellationToken, PdfTranslationConfig, PdfTranslationExportRequest,
    PdfTranslationHoverRegion, PdfTranslationPageFailure, PdfTranslationProgress,
    PdfTranslationService, PdfTranslationSessionRequest, PdfTranslationSessionResult,
    PdfTranslationSessionUpdate,
};
use glyphst_settings::Settings;
use glyphst_translation::{
    LanguageCode, TranslationExecutionConfig, TranslationRequest, TranslationService,
};

/// Translations pinned to the Translation panel.
const MAX_PINNED: usize = 20;

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
    settings: Option<Arc<Settings>>,
    settings_rx: Option<Receiver<(Settings, Vec<String>)>>,
    /// Problems reading the settings or keys.
    pub warnings: Vec<String>,
    results_tx: Sender<(u64, Result<String, String>)>,
    results_rx: Receiver<(u64, Result<String, String>)>,
    next_card: u64,
    pub card: Option<Card>,
    /// Pinned translations, newest first: source and translation.
    pub pinned: Vec<(String, String)>,
}

impl Translator {
    /// Starts reading the user's settings and keys, or uses `settings`
    /// (scripted runs and tests). `persist` keeps translated text and
    /// translation jobs where the GPUI Reader keeps them; otherwise they
    /// stay in memory and a temporary directory. Typst typesets in a worker
    /// process of this executable (see `main`), or in this process for
    /// tests.
    pub fn new(
        ctx: &egui::Context,
        settings: Option<Settings>,
        persist: bool,
        typst_in_process: bool,
    ) -> Self {
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
        let (settings, settings_rx) = match settings {
            Some(settings) => (Some(Arc::new(settings)), None),
            None => (None, Some(load_settings(ctx.clone()))),
        };
        let (results_tx, results_rx) = mpsc::channel();
        Self {
            text,
            pdf,
            settings,
            settings_rx,
            warnings,
            results_tx,
            results_rx,
            next_card: 1,
            card: None,
            pinned: Vec::new(),
        }
    }

    /// Takes the loaded settings and finished translations.
    pub fn poll(&mut self) {
        if let Some((settings, warnings)) = self
            .settings_rx
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok())
        {
            self.settings = Some(Arc::new(settings));
            self.warnings.extend(warnings);
            self.settings_rx = None;
        }
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

    /// Starts translating a PDF into `target`.
    pub fn start_session(
        &self,
        path: &Path,
        target: Option<&str>,
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
        let config = PdfTranslationConfig::from_settings(&settings);
        let cancel = CancellationToken::new();
        let (events_tx, events) = mpsc::channel();
        let (service, worker_cancel, ctx) = (self.pdf.clone(), cancel.clone(), ctx.clone());
        let spawned = thread::Builder::new()
            .name("pdf-translation".into())
            .spawn(move || {
                let progress_tx = events_tx.clone();
                let progress_ctx = ctx.clone();
                let update_tx = events_tx.clone();
                let update_ctx = ctx.clone();
                let result = service.run_session_from_snapshot(
                    config,
                    request,
                    snapshot,
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
            previews: HashMap::new(),
            failures: Vec::new(),
            result: None,
            show_translated: true,
            cancel,
            events,
            export: None,
        })
    }

    /// Writes a finished session's translated PDF to `output`.
    pub fn export(
        &self,
        session: &mut DocSession,
        output: PathBuf,
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
        "glyphst-reader-egui-translations-{}",
        std::process::id()
    ))
}

/// Reads the user's settings and keychain keys on a thread.
fn load_settings(ctx: egui::Context) -> Receiver<(Settings, Vec<String>)> {
    let (sender, receiver) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("settings".into())
        .spawn(move || {
            let mut settings = glyphst_settings::load_user_settings();
            let warnings = glyphst_settings::read_keychain_secrets(&mut settings);
            let _ = sender.send((settings, warnings));
            ctx.request_repaint();
        });
    if spawned.is_err() {
        eprintln!("could not start reading the settings");
    }
    receiver
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

/// A translated page: its typeset PDF and where its blocks came from.
#[derive(Clone, Debug)]
pub struct Preview {
    pub revision: u64,
    pub pdf: Arc<[u8]>,
    pub hover: Vec<PdfTranslationHoverRegion>,
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
    pub previews: HashMap<usize, Preview>,
    pub failures: Vec<PdfTranslationPageFailure>,
    pub result: Option<PdfTranslationSessionResult>,
    /// Translated pages are shown in place of the originals.
    pub show_translated: bool,
    cancel: CancellationToken,
    events: Receiver<SessionEvent>,
    pub export: Option<Export>,
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
                        page_count,
                        ready_prefix,
                        ..
                    } => {
                        self.page_count = page_count;
                        self.ready_pages = self.ready_pages.max(ready_prefix);
                    }
                    PdfTranslationSessionUpdate::PagePreviewReady(preview) => {
                        let newer = self
                            .previews
                            .get(&preview.page_index)
                            .is_none_or(|current| current.revision < preview.revision);
                        if newer {
                            self.previews.insert(
                                preview.page_index,
                                Preview {
                                    revision: preview.revision,
                                    pdf: preview.pdf.clone(),
                                    hover: preview.hover_regions.clone(),
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
                    PdfTranslationSessionUpdate::CacheReady(_)
                    | PdfTranslationSessionUpdate::UnitsChanged(_)
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
            SessionStatus::Running => match &self.progress {
                Some(progress) => {
                    let count = match (progress.completed, progress.total) {
                        (Some(done), Some(total)) => format!(" {done}/{total}"),
                        _ => String::new(),
                    };
                    format!("{}{count}", stage_label(progress))
                }
                None => "Starting…".into(),
            },
        }
    }
}

impl Drop for DocSession {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
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

    use super::*;
    use crate::fake_providers::{fake_translation, start_translation_server, translation_settings};

    fn wait_until(mut done: impl FnMut() -> bool) {
        let started = Instant::now();
        while !done() {
            assert!(started.elapsed() < Duration::from_secs(180), "timed out");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn translator(ctx: &egui::Context) -> Translator {
        let port = start_translation_server().unwrap();
        Translator::new(ctx, Some(translation_settings(port)), false, true)
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

    #[test]
    fn documents_translate_into_typeset_pages() {
        let directory = scratch_artifacts();
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("source.pdf");
        std::fs::write(
            &path,
            glyphst_pdf::test_pdf::text_pdf(&["A short document.", "It has two lines."]),
        )
        .unwrap();
        let ctx = egui::Context::default();
        let translator = translator(&ctx);
        let mut session = translator.start_session(&path, None, &ctx).unwrap();
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
        session.show_translated = false;
        assert!(session.shown_preview(0).is_none(), "the original is shown");
        let _ = std::fs::remove_dir_all(&directory);
    }
}
