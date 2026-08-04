use std::collections::{BTreeSet, HashMap, HashSet, hash_map::DefaultHasher};
use std::fmt::{self, Write as _};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering as AtomicOrdering},
    mpsc as sync_mpsc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, anyhow};
use glyphst_settings::{OcrMode, OcrProvider, PdfContentReviewMode, Settings};
use glyphst_translation::{LanguageCode, TranslationService};
use mupdf::pdf::PdfDocument as MuPdfDocument;
use mupdf::{Document, TextPageFlags};
use serde::{Deserialize, Serialize};

use glyphst_pdf_translation::{
    BlockKind, BoundingBox, CancellationToken, DocumentMetadata, DocumentV1, LayoutDocument,
    LayoutOptions, LayoutPage, OcrFusionReport, PaddleClient, PaddleClientConfig, Page,
    PdfComposer, PdfInspection, PdfTranslationStage, ProgressTracker, ProtectedRegionKind,
    TranslatedDocument, TranslationProgressSink, TypstCompiler, TypstCompilerConfig,
    UnlimitedClient, UnlimitedClientConfig, annotate_reference_sections, build_layout_with_options,
    extract_native_document, fuse_native_with_ocr_report, inspect_pdf, normalize_paddle_payload,
    normalize_unlimited_payload, reconcile_provider_continuations, review_translation_policies,
    sanitize_source_pdf, source_text_regions,
};
use glyphst_pdf_translation::{
    TranslatedUnit, TranslationDocumentError, TranslationFailureDiagnostic,
    TranslationFailurePhase, TranslationQueueStats, TranslationRunProgress, TranslationRunSnapshot,
    translate_document_partial_resuming,
};

use crate::backend::GlyphstTranslationBackend;
use crate::reviewer::GlyphstPolicyReviewer;

const ARTIFACT_VERSION: u32 = 7;
const PIPELINE_VERSION: &str = "native-rust-v26-page-streaming-canonical-previews";
const CHECKPOINT_VERSION: u32 = 4;

#[derive(Clone)]
pub struct PdfTranslationService {
    translation: TranslationService,
    typst: TypstCompiler,
    composer: PdfComposer,
    artifact_cache_state: Arc<Mutex<ArtifactCacheState>>,
}

#[derive(Default)]
struct ArtifactCacheState {
    active_runs: usize,
}

struct ArtifactRunGuard {
    state: Arc<Mutex<ArtifactCacheState>>,
}

impl Drop for ArtifactRunGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.active_runs = state.active_runs.saturating_sub(1);
        }
    }
}

impl gpui::Global for PdfTranslationService {}

impl PdfTranslationService {
    pub fn new(translation: TranslationService) -> Self {
        Self::with_typst_config(
            translation,
            TypstCompilerConfig::current_executable_worker(),
        )
    }

    pub fn new_in_process(translation: TranslationService) -> Self {
        Self::with_typst_config(translation, TypstCompilerConfig::default())
    }

    fn with_typst_config(
        translation: TranslationService,
        typst_config: TypstCompilerConfig,
    ) -> Self {
        Self {
            translation,
            typst: TypstCompiler::new(typst_config),
            composer: PdfComposer,
            artifact_cache_state: Arc::new(Mutex::new(ArtifactCacheState::default())),
        }
    }

    pub fn clear_translation_data(&self) -> anyhow::Result<()> {
        self.clear_translation_data_at(&artifact_root(), &legacy_artifact_root(), true)
    }

    fn clear_translation_data_at(
        &self,
        artifact_root: &Path,
        legacy_artifact_root: &Path,
        clear_provider_cache: bool,
    ) -> anyhow::Result<()> {
        let state = self
            .artifact_cache_state
            .lock()
            .map_err(|_| anyhow!("PDF translation cache state is unavailable"))?;
        if state.active_runs > 0 {
            return Err(anyhow!(
                "A PDF translation is active. Cancel it or wait for it to finish before clearing translation data."
            ));
        }

        let mut errors = Vec::new();
        if clear_provider_cache && let Err(error) = self.translation.clear_cache() {
            errors.push(format!(
                "could not clear cached provider responses: {error:#}"
            ));
        }
        for (label, path) in [
            ("PDF translation artifacts", artifact_root),
            (
                "legacy document translation artifacts",
                legacy_artifact_root,
            ),
        ] {
            if let Err(error) = remove_artifact_directory(path) {
                errors.push(format!("could not clear {label}: {error:#}"));
            }
        }
        drop(state);

        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(errors.join("; ")))
        }
    }

    fn begin_run(&self) -> Result<ArtifactRunGuard, PdfTranslationError> {
        let mut state = self.artifact_cache_state.lock().map_err(|_| {
            PdfTranslationError::message(
                PdfTranslationStage::Preparing,
                "PDF translation cache state is unavailable",
            )
        })?;
        state.active_runs = state.active_runs.saturating_add(1);
        drop(state);
        Ok(ArtifactRunGuard {
            state: self.artifact_cache_state.clone(),
        })
    }

    pub fn run(
        &self,
        settings: Settings,
        request: PdfTranslationRequest,
        cancellation: CancellationToken,
        progress_sink: impl TranslationProgressSink + 'static,
    ) -> Result<PdfTranslationResult, PdfTranslationError> {
        let _run_guard = self.begin_run()?;
        let mut progress = ProgressTracker::new(progress_sink);
        let session_request = PdfTranslationSessionRequest {
            source_path: request.source_path.clone(),
            source_language: request.source_language.clone(),
            target_language: request.target_language.clone(),
        };
        let mut ignore_update = |_: PdfTranslationSessionUpdate| {};
        let session = self.run_session_inner(
            &settings,
            session_request,
            &cancellation,
            &mut progress,
            &mut ignore_update,
        )?;
        self.export_session_inner(
            &settings,
            &session,
            PdfTranslationExportRequest {
                output_path: request.output_path,
                original_unit_ids: Vec::new(),
            },
            &cancellation,
            &mut progress,
        )
    }

    pub fn run_session(
        &self,
        settings: Settings,
        request: PdfTranslationSessionRequest,
        cancellation: CancellationToken,
        progress_sink: impl TranslationProgressSink + 'static,
        mut update_sink: impl FnMut(PdfTranslationSessionUpdate) + Send,
    ) -> Result<PdfTranslationSessionResult, PdfTranslationError> {
        let _run_guard = self.begin_run()?;
        let mut progress = ProgressTracker::new(progress_sink);
        self.run_session_inner(
            &settings,
            request,
            &cancellation,
            &mut progress,
            &mut update_sink,
        )
    }

    pub fn export_session(
        &self,
        settings: Settings,
        session: PdfTranslationSessionResult,
        request: PdfTranslationExportRequest,
        cancellation: CancellationToken,
        progress_sink: impl TranslationProgressSink + 'static,
    ) -> Result<PdfTranslationResult, PdfTranslationError> {
        let _run_guard = self.begin_run()?;
        let mut progress = ProgressTracker::new(progress_sink);
        self.export_session_inner(&settings, &session, request, &cancellation, &mut progress)
    }

    /// Rebuild one live page after a session-local original/translated toggle.
    /// This deliberately uses the same finalizer as batch output and streaming
    /// previews, so the interaction cannot diverge into a UI text overlay.
    pub fn render_session_page_preview(
        &self,
        session: &PdfTranslationSessionResult,
        page_index: usize,
        revision: u64,
        original_unit_ids: &[String],
        cancellation: &CancellationToken,
    ) -> Result<PdfTranslationPagePreview, PdfTranslationError> {
        let _run_guard = self.begin_run()?;
        self.compile_page_preview(
            &session.translated,
            &session.source_snapshot_path,
            &session.inspection,
            &session.job_directory,
            page_index,
            revision,
            &original_unit_ids.iter().cloned().collect(),
            cancellation,
        )
    }

    fn run_session_inner(
        &self,
        settings: &Settings,
        session_request: PdfTranslationSessionRequest,
        cancellation: &CancellationToken,
        progress: &mut ProgressTracker,
        update_sink: &mut (impl FnMut(PdfTranslationSessionUpdate) + Send),
    ) -> Result<PdfTranslationSessionResult, PdfTranslationError> {
        let request = PdfTranslationRequest {
            source_path: session_request.source_path.clone(),
            output_path: PathBuf::new(),
            source_language: session_request.source_language.clone(),
            target_language: session_request.target_language.clone(),
        };
        let (fingerprint, source_fingerprint, source_bytes) =
            request_fingerprint_with_source(settings, &request)?;
        let job_directory = create_job_directory(&fingerprint)?;
        let prepared_directory = create_prepared_directory(&source_fingerprint)?;
        let source_path = job_directory.join("source-original.pdf");
        fs::write(&source_path, source_bytes).map_err(|error| {
            PdfTranslationError::stage(
                PdfTranslationStage::Preparing,
                "Could not snapshot the source PDF",
                error,
            )
        })?;
        let mut pipeline_request = request.clone();
        pipeline_request.source_path = source_path;
        progress.report(
            PdfTranslationStage::Preparing,
            "Inspecting source PDF",
            Some(0),
            Some(1),
        );
        cancellation
            .check()
            .map_err(|_| PdfTranslationError::cancelled(PdfTranslationStage::Preparing))?;
        let inspection =
            inspect_pdf(&pipeline_request.source_path, &cancellation).map_err(|error| {
                PdfTranslationError::stage(
                    PdfTranslationStage::Preparing,
                    "Could not inspect the source PDF",
                    error,
                )
            })?;
        let target_pages = inspection
            .pages
            .iter()
            .map(|page| crate::PageGeometry::new(page.width, page.height))
            .collect::<Vec<_>>();
        progress.finish_stage(PdfTranslationStage::Preparing, "Source PDF inspected");
        let backend = GlyphstTranslationBackend::new(
            self.translation.clone(),
            settings.clone(),
            request.source_language.clone(),
            request.target_language.clone(),
        );
        let mut streamed_units = Vec::<TranslatedUnit>::new();
        let mut streamed_preview_units = HashMap::<String, (u64, Vec<usize>)>::new();
        let mut streamed_preview_revisions = HashMap::<usize, u64>::new();

        let document_path = prepared_directory.join("document.v1.json");
        let (mut document, reused_prepared_document) = if let Some(mut document) =
            load_normalized_document(&document_path, &source_fingerprint)
        {
            document.metadata.source_path = pipeline_request.source_path.clone();
            progress.finish_stage(PdfTranslationStage::Ocr, "Reused extracted document");
            progress.finish_stage(
                PdfTranslationStage::Normalizing,
                "Reused normalized document",
            );
            (document, true)
        } else {
            let all_pages = (0..target_pages.len()).collect::<Vec<_>>();
            let automatic_ocr_pages = inspection
                .pages
                .iter()
                .filter(|page| page.needs_ocr)
                .map(|page| page.page_index)
                .collect::<Vec<_>>();
            let document = match settings.ocr.mode {
                OcrMode::Always => self.run_selected_ocr(
                    settings,
                    &pipeline_request,
                    &target_pages,
                    &prepared_directory,
                    &job_directory,
                    &source_fingerprint,
                    &inspection,
                    &all_pages,
                    cancellation,
                    progress,
                    update_sink,
                    &backend,
                    &mut streamed_units,
                    &mut streamed_preview_units,
                    &mut streamed_preview_revisions,
                )?,
                OcrMode::Auto if inspection.recommends_ocr => self.run_selected_ocr(
                    settings,
                    &pipeline_request,
                    &target_pages,
                    &prepared_directory,
                    &job_directory,
                    &source_fingerprint,
                    &inspection,
                    &automatic_ocr_pages,
                    cancellation,
                    progress,
                    update_sink,
                    &backend,
                    &mut streamed_units,
                    &mut streamed_preview_units,
                    &mut streamed_preview_revisions,
                )?,
                OcrMode::Auto | OcrMode::Off => {
                    progress.report(
                        PdfTranslationStage::Ocr,
                        "Extracting editable PDF text with MuPDF",
                        None,
                        None,
                    );
                    let document = extract_native_document(
                        &pipeline_request.source_path,
                        source_fingerprint.clone(),
                        cancellation,
                    )
                    .map_err(|error| {
                        PdfTranslationError::stage(
                            PdfTranslationStage::Ocr,
                            "Could not extract native PDF text",
                            error,
                        )
                    })?;
                    progress.finish_stage(PdfTranslationStage::Ocr, "Native PDF text extracted");
                    progress.finish_stage(
                        PdfTranslationStage::Normalizing,
                        "Native document normalized",
                    );
                    document
                }
            };
            (document, false)
        };
        if !reused_prepared_document {
            annotate_reference_sections(&mut document);
        }
        let mut warnings = Vec::new();
        if !reused_prepared_document
            && let Some(reviewer) = GlyphstPolicyReviewer::from_settings(settings)
        {
            let review_errors = review_translation_policies(&mut document, &reviewer);
            if !review_errors.is_empty() {
                document.metadata.extra.insert(
                    "policy_review_errors".to_string(),
                    serde_json::json!(
                        review_errors
                            .iter()
                            .map(|error| error.message.as_str())
                            .collect::<Vec<_>>()
                    ),
                );
                let affected_block_ids = document
                    .pages
                    .iter()
                    .flat_map(|page| &page.blocks)
                    .filter(|block| {
                        block.policy.reason.starts_with("policy_review_failed:")
                            || block.policy.reason.starts_with("policy_review_incomplete:")
                    })
                    .map(|block| block.id.clone())
                    .collect();
                let detail = review_errors
                    .first()
                    .map(|error| error.message.trim().chars().take(300).collect::<String>())
                    .unwrap_or_default();
                warnings.push(PdfTranslationWarning {
                    code: "content_review_failed".to_string(),
                    message: format!(
                        "AI content review failed for {} page batch(es); ambiguous text was preserved. {detail}",
                        review_errors.len()
                    ),
                    affected_block_ids,
                });
            }
        } else if let Some(errors) = document
            .metadata
            .extra
            .get("policy_review_errors")
            .and_then(serde_json::Value::as_array)
            && !errors.is_empty()
        {
            warnings.push(PdfTranslationWarning {
                code: "content_review_failed".to_string(),
                message: format!(
                    "AI content review previously failed for {} page batch(es); ambiguous text was preserved.",
                    errors.len()
                ),
                affected_block_ids: document
                    .pages
                    .iter()
                    .flat_map(|page| &page.blocks)
                    .filter(|block| {
                        block.policy.reason.starts_with("policy_review_failed:")
                            || block.policy.reason.starts_with("policy_review_incomplete:")
                    })
                    .map(|block| block.id.clone())
                    .collect(),
            });
        }
        if !reused_prepared_document {
            write_json_atomically(&document_path, &document, PdfTranslationStage::Normalizing)?;
        }
        let ocr_page_failures = ocr_page_failures(&document);
        let ocr_failures = ocr_failure_diagnostics(&document);
        for failure in &ocr_page_failures {
            update_sink(PdfTranslationSessionUpdate::PageFailed(failure.clone()));
        }
        let page_count = document.pages.len();
        for page in &document.pages {
            update_sink(PdfTranslationSessionUpdate::PagePrepared {
                metadata: document.metadata.clone(),
                page: page.clone(),
                page_count,
                ready_prefix: page.page_index + 1,
            });
        }

        progress.report(
            PdfTranslationStage::Translating,
            "Preparing semantic translation units",
            Some(0),
            Some(1),
        );
        let checkpoint_identity =
            TranslationCheckpointIdentity::new(&settings, &request, &fingerprint, &document);
        let checkpoint_path = job_directory.join("translated-document.partial.json");
        let diagnostics_path = job_directory.join("translation-diagnostics.json");
        let block_locations = translation_block_locations(&document);
        let mut resumed_by_id = load_translation_checkpoint(&checkpoint_path, &checkpoint_identity)
            .into_iter()
            .map(|unit| (unit.id.clone(), unit))
            .collect::<HashMap<_, _>>();
        for unit in &streamed_units {
            resumed_by_id.insert(unit.id.clone(), unit.clone());
        }
        let resumed_units = resumed_by_id.into_values().collect::<Vec<_>>();
        let mut last_checkpoint_resolved = resumed_units.len();
        let mut last_terminal_failures = 0usize;
        let mut final_translation_counts = TranslationRunProgress::default();
        let mut final_translation_snapshot = None;
        let streamed_hashes = streamed_units
            .iter()
            .map(|unit| (unit.id.clone(), unit.semantic_hash))
            .collect::<HashMap<_, _>>();
        let mut final_reconciliation_emitted = false;
        let update_sink = Mutex::new(update_sink);
        let translation_result = std::thread::scope(|scope| {
            let (preview_tx, preview_rx) = sync_mpsc::channel();
            scope.spawn(|| {
                stream_page_previews(
                    self,
                    &document,
                    &pipeline_request.source_path,
                    &inspection,
                    &job_directory,
                    &cancellation,
                    streamed_preview_units.clone(),
                    streamed_preview_revisions.clone(),
                    preview_rx,
                    &update_sink,
                );
            });
            let result = translate_document_partial_resuming(
                document.clone(),
                &backend,
                &cancellation,
                resumed_units,
                |snapshot| {
                    let counts = &snapshot.progress;
                    final_translation_counts = counts.clone();
                    final_translation_snapshot = Some(snapshot.clone());
                    {
                        let mut sink = update_sink
                            .lock()
                            .map_err(|_| "translation session update sink stopped".to_string())?;
                        if !final_reconciliation_emitted {
                            let valid_hashes = snapshot
                                .resolved_units
                                .iter()
                                .map(|unit| (unit.id.as_str(), unit.semantic_hash))
                                .collect::<HashMap<_, _>>();
                            let mut invalidated_unit_ids = streamed_hashes
                                .iter()
                                .filter(|(id, semantic_hash)| {
                                    valid_hashes
                                        .get(id.as_str())
                                        .is_none_or(|valid| valid != *semantic_hash)
                                })
                                .map(|(id, _)| id.clone())
                                .collect::<Vec<_>>();
                            invalidated_unit_ids.sort();
                            (**sink)(PdfTranslationSessionUpdate::Reconciled {
                                ready_prefix: document.pages.len(),
                                invalidated_unit_ids,
                                final_pass: true,
                            });
                            final_reconciliation_emitted = true;
                        }
                        (**sink)(PdfTranslationSessionUpdate::UnitsChanged(snapshot.clone()));
                    }
                    preview_tx
                        .send(snapshot.clone())
                        .map_err(|_| "page preview worker stopped".to_string())?;
                    let resolved = snapshot.resolved_units.len();
                    let should_persist = translation_checkpoint_changed(
                        resolved,
                        last_checkpoint_resolved,
                        counts.failed,
                        last_terminal_failures,
                    );
                    if should_persist {
                        write_translation_checkpoint(
                            &checkpoint_path,
                            &checkpoint_identity,
                            snapshot,
                        )
                        .map_err(|error| error.message)?;
                        write_translation_diagnostics(
                            &diagnostics_path,
                            &checkpoint_identity,
                            snapshot,
                        )
                        .map_err(|error| error.message)?;
                        last_checkpoint_resolved = resolved;
                        last_terminal_failures = counts.failed;
                    }
                    progress.report_translation(
                        format!(
                            "Translated {}, cached {}, retrying {}, failed {} of {} semantic units",
                            counts.translated,
                            counts.cached,
                            counts.retrying,
                            counts.failed,
                            counts.total
                        ),
                        counts.translated as u32,
                        counts.cached as u32,
                        counts.retrying as u32,
                        counts.failed as u32,
                        counts.total as u32,
                    );
                    Ok(())
                },
            );
            drop(preview_tx);
            result
        });
        let mut outcome = match translation_result {
            Ok(outcome) => outcome,
            Err(error) => {
                let detailed_message = translation_failure_report(
                    &request,
                    &fingerprint,
                    &job_directory,
                    &diagnostics_path,
                    &checkpoint_identity,
                    &block_locations,
                    final_translation_snapshot.as_ref(),
                    &error,
                );
                return Err(PdfTranslationError {
                    stage: PdfTranslationStage::Translating,
                    message: error.message,
                    retryable: error.retryable,
                    detailed_message: Some(detailed_message),
                });
            }
        };
        outcome.failures.extend(ocr_failures);
        let prepared = prepare_translation_layout(&outcome.document, &HashSet::new());
        if !prepared.failures.is_empty() {
            outcome.progress.failed = outcome
                .progress
                .failed
                .saturating_add(prepared.failures.len());
            outcome.failures.extend(prepared.failures);
        }
        outcome.document = prepared.translated;
        progress.report_translation(
            if outcome.failures.is_empty() {
                "Document translation complete"
            } else {
                "Document translation complete with untranslated units"
            },
            final_translation_counts.translated as u32,
            final_translation_counts.cached as u32,
            0,
            final_translation_counts.failed as u32,
            final_translation_counts.total as u32,
        );
        write_json(
            job_directory.join("translated-document.json"),
            &outcome.document,
        )?;
        if let Ok(mut sink) = update_sink.lock() {
            (**sink)(PdfTranslationSessionUpdate::Finished);
        }
        Ok(PdfTranslationSessionResult {
            request: session_request,
            translated: outcome.document,
            failures: outcome.failures,
            progress: outcome.progress,
            queue: outcome.queue,
            inspection,
            source_snapshot_path: pipeline_request.source_path,
            job_directory,
            fingerprint,
            warnings,
        })
    }

    fn export_session_inner(
        &self,
        settings: &Settings,
        session: &PdfTranslationSessionResult,
        request: PdfTranslationExportRequest,
        cancellation: &CancellationToken,
        progress: &mut ProgressTracker,
    ) -> Result<PdfTranslationResult, PdfTranslationError> {
        let job_directory = &session.job_directory;
        let source_snapshot_path = &session.source_snapshot_path;
        let original_unit_ids = request
            .original_unit_ids
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let mut warnings = session.warnings.clone();
        if !session.failures.is_empty() {
            warnings.push(PdfTranslationWarning {
                code: "translation_units_failed".to_string(),
                message: format!(
                    "{} translation unit(s) remain in the original language.",
                    session.failures.len()
                ),
                affected_block_ids: session
                    .failures
                    .iter()
                    .flat_map(|failure| failure.member_block_ids.iter().cloned())
                    .collect(),
            });
        }
        progress.report(
            PdfTranslationStage::Layout,
            "Fitting translated text to source geometry",
            Some(0),
            Some(1),
        );
        let prepared = prepare_translation_layout(&session.translated, &original_unit_ids);
        let translated = prepared.translated;
        let layout = prepared.layout;
        let mut artifact_failures = session.failures.clone();
        if !prepared.failures.is_empty() {
            warnings.push(PdfTranslationWarning {
                code: "translation_layout_fallback".to_string(),
                message: format!(
                    "{} translated unit(s) could not be laid out safely and remain in the original language.",
                    prepared.failures.len()
                ),
                affected_block_ids: prepared
                    .failures
                    .iter()
                    .flat_map(|failure| failure.member_block_ids.iter().cloned())
                    .collect(),
            });
            artifact_failures.extend(prepared.failures);
        }
        validate_layout_completeness(&translated, &layout)?;
        write_json(job_directory.join("layout.json"), &layout)?;
        progress.finish_stage(PdfTranslationStage::Layout, "Translated layout prepared");

        progress.report(
            PdfTranslationStage::Compiling,
            "Compiling the translated PDF with Typst",
            None,
            None,
        );
        let composed_path = self.finalize_layout_pdf(
            source_snapshot_path,
            source_snapshot_path,
            &layout,
            &session.inspection,
            job_directory,
            cancellation,
        )?;
        write_json(job_directory.join("layout.json"), &layout)?;
        progress.finish_stage(PdfTranslationStage::Compiling, "Typst PDF compiled");

        progress.report(
            PdfTranslationStage::Composing,
            "Restoring PDF links, outlines, and metadata",
            Some(0),
            Some(1),
        );
        progress.finish_stage(PdfTranslationStage::Composing, "PDF structure restored");

        progress.report(
            PdfTranslationStage::Validating,
            "Validating translated PDF",
            Some(0),
            Some(1),
        );
        cancellation
            .check()
            .map_err(|_| PdfTranslationError::cancelled(PdfTranslationStage::Validating))?;
        publish_atomically(&composed_path, &request.output_path)?;
        let artifact_request = PdfTranslationRequest {
            source_path: session.request.source_path.clone(),
            output_path: request.output_path.clone(),
            source_language: session.request.source_language.clone(),
            target_language: session.request.target_language.clone(),
        };
        let manifest = build_manifest(
            settings,
            &artifact_request,
            &session.fingerprint,
            job_directory,
            &layout,
            &translated,
            &warnings,
            &artifact_failures,
            &request.original_unit_ids,
        );
        let manifest_path = job_directory.join("artifact-manifest.json");
        write_json(&manifest_path, &manifest)?;
        progress.finish("Translated PDF is ready");

        Ok(PdfTranslationResult {
            output_path: request.output_path,
            manifest_path,
            job_directory: session.job_directory.clone(),
            fingerprint: session.fingerprint.clone(),
            warnings,
        })
    }

    fn finalize_layout_pdf(
        &self,
        original_source_path: &Path,
        render_source_path: &Path,
        layout: &LayoutDocument,
        inspection: &PdfInspection,
        work_directory: &Path,
        cancellation: &CancellationToken,
    ) -> Result<PathBuf, PdfTranslationError> {
        create_private_directory(work_directory).map_err(|error| {
            PdfTranslationError::stage(
                PdfTranslationStage::Compiling,
                "Could not create the translated PDF render directory",
                error,
            )
        })?;
        let sanitized_source_path = work_directory.join("source-sanitized.pdf");
        sanitize_source_pdf(
            render_source_path,
            &sanitized_source_path,
            layout,
            cancellation,
        )
        .map_err(|error| PdfTranslationError {
            stage: PdfTranslationStage::Compiling,
            message: format!("Could not remove source text from translated regions: {error}"),
            retryable: false,
            detailed_message: None,
        })?;
        let typst_output = self
            .typst
            .compile_translated_pdf(
                original_source_path,
                &sanitized_source_path,
                layout,
                work_directory,
                cancellation,
            )
            .map_err(|error| PdfTranslationError {
                stage: PdfTranslationStage::Compiling,
                message: error.message,
                retryable: false,
                detailed_message: None,
            })?;
        let composed_path = work_directory.join("translated-final.pdf");
        self.composer
            .compose(
                original_source_path,
                &typst_output.pdf_path,
                &composed_path,
                layout,
                inspection,
                cancellation,
            )
            .map_err(|error| PdfTranslationError {
                stage: PdfTranslationStage::Composing,
                message: error.message,
                retryable: false,
                detailed_message: None,
            })?;
        validate_rendered_output(original_source_path, &composed_path, layout)?;
        cancellation
            .check()
            .map_err(|_| PdfTranslationError::cancelled(PdfTranslationStage::Validating))?;
        Ok(composed_path)
    }

    fn compile_page_preview(
        &self,
        translated: &TranslatedDocument,
        source_path: &Path,
        inspection: &PdfInspection,
        job_directory: &Path,
        page_index: usize,
        revision: u64,
        original_unit_ids: &HashSet<String>,
        cancellation: &CancellationToken,
    ) -> Result<PdfTranslationPagePreview, PdfTranslationError> {
        let prepared = prepare_translation_layout(translated, original_unit_ids);
        let page = prepared
            .layout
            .pages
            .iter()
            .find(|page| page.page_index == page_index)
            .cloned()
            .ok_or_else(|| {
                PdfTranslationError::message(
                    PdfTranslationStage::Compiling,
                    format!("Translated preview page {} is unavailable", page_index + 1),
                )
            })?;
        let hover_regions = build_page_hover_regions(translated, &page);
        let source_bytes = extract_single_pdf_page(source_path, page_index)?;
        let work_directory = job_directory
            .join("page-previews")
            .join(format!("page-{:04}-revision-{revision:08}", page_index + 1));
        create_private_directory(&work_directory).map_err(|error| {
            PdfTranslationError::stage(
                PdfTranslationStage::Compiling,
                "Could not create the page preview directory",
                error,
            )
        })?;
        let page_source_path = work_directory.join("source-page.pdf");
        fs::write(&page_source_path, source_bytes).map_err(|error| {
            PdfTranslationError::stage(
                PdfTranslationStage::Compiling,
                "Could not stage a source page for preview",
                error,
            )
        })?;
        let mut page_profile = inspection.pages.get(page_index).cloned().ok_or_else(|| {
            PdfTranslationError::message(
                PdfTranslationStage::Compiling,
                format!("Source profile for page {} is unavailable", page_index + 1),
            )
        })?;
        page_profile.page_index = 0;
        let page_inspection = PdfInspection {
            recommends_ocr: page_profile.needs_ocr,
            pages: vec![page_profile],
        };
        let preview_layout = reindex_preview_layout(&page);
        let output_path = self.finalize_layout_pdf(
            &page_source_path,
            &page_source_path,
            &preview_layout,
            &page_inspection,
            &work_directory,
            cancellation,
        )?;
        let pdf = fs::read(&output_path).map_err(|error| {
            PdfTranslationError::stage(
                PdfTranslationStage::Compiling,
                "Could not load the compiled page preview",
                error,
            )
        })?;
        let page_block_ids = translated
            .document
            .pages
            .get(page_index)
            .map(|page| {
                page.blocks
                    .iter()
                    .map(|block| block.id.as_str())
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        let layout_failures = prepared
            .failures
            .into_iter()
            .filter(|failure| {
                failure
                    .member_block_ids
                    .iter()
                    .any(|block_id| page_block_ids.contains(block_id.as_str()))
            })
            .collect();
        Ok(PdfTranslationPagePreview {
            page_index,
            revision,
            layout: page,
            hover_regions,
            pdf: Arc::from(pdf),
            layout_failures,
        })
    }

    pub fn cached_artifact(
        &self,
        settings: &Settings,
        request: &PdfTranslationRequest,
    ) -> Option<PdfTranslationResult> {
        let _state = self.artifact_cache_state.lock().ok()?;
        let fingerprint = request_fingerprint(settings, request).ok()?;
        let job_directory = artifact_root().join(&fingerprint);
        let manifest_path = job_directory.join("artifact-manifest.json");
        let manifest: PdfTranslationArtifactManifest =
            serde_json::from_slice(&fs::read(&manifest_path).ok()?).ok()?;
        if manifest.version != ARTIFACT_VERSION || manifest.pipeline_version != PIPELINE_VERSION {
            return None;
        }
        if manifest
            .warnings
            .iter()
            .any(|warning| warning.code == "content_review_failed")
        {
            // A warning artifact is still useful output, but it must not make a
            // later user-initiated retry bypass the optional reviewer forever.
            return None;
        }
        let output_path = PathBuf::from(manifest.output_path);
        output_path.is_file().then_some(PdfTranslationResult {
            output_path,
            manifest_path,
            job_directory,
            fingerprint,
            warnings: manifest.warnings,
        })
    }

    pub fn artifact_request(
        &self,
        settings: &Settings,
        source_path: PathBuf,
        source_language: Option<LanguageCode>,
        target_language: LanguageCode,
    ) -> Result<PdfTranslationRequest, PdfTranslationError> {
        let provisional = PdfTranslationRequest {
            output_path: source_path.with_extension(format!("{}.pdf", target_language.as_str())),
            source_path,
            source_language,
            target_language,
        };
        let fingerprint = request_fingerprint(settings, &provisional)?;
        Ok(PdfTranslationRequest {
            output_path: artifact_root().join(fingerprint).join("translated.pdf"),
            ..provisional
        })
    }

    fn run_selected_ocr(
        &self,
        settings: &Settings,
        request: &PdfTranslationRequest,
        target_pages: &[crate::PageGeometry],
        preparation_directory: &Path,
        preview_job_directory: &Path,
        document_id: &str,
        inspection: &PdfInspection,
        selected_pages: &[usize],
        cancellation: &CancellationToken,
        progress: &mut ProgressTracker,
        update_sink: &mut impl FnMut(PdfTranslationSessionUpdate),
        backend: &GlyphstTranslationBackend,
        streamed_units: &mut Vec<TranslatedUnit>,
        streamed_preview_units: &mut HashMap<String, (u64, Vec<usize>)>,
        streamed_preview_revisions: &mut HashMap<usize, u64>,
    ) -> Result<DocumentV1, PdfTranslationError> {
        cancellation
            .check()
            .map_err(|_| PdfTranslationError::cancelled(PdfTranslationStage::Ocr))?;
        let native = extract_native_document(&request.source_path, document_id, cancellation)
            .map_err(|error| {
                PdfTranslationError::stage(
                    PdfTranslationStage::Ocr,
                    "Could not extract native PDF text for OCR fusion",
                    error,
                )
            })?;
        let selected_pages = selected_pages
            .iter()
            .copied()
            .filter(|page_index| *page_index < native.pages.len())
            .collect::<Vec<_>>();
        if selected_pages.is_empty() {
            progress.finish_stage(PdfTranslationStage::Ocr, "Native PDF text extracted");
            progress.finish_stage(
                PdfTranslationStage::Normalizing,
                "Native document normalized",
            );
            return Ok(native);
        }

        progress.report(
            PdfTranslationStage::Ocr,
            format!("Preparing {} page(s) for bounded OCR", selected_pages.len()),
            Some(0),
            Some(selected_pages.len() as u32),
        );
        let client = PageOcrClient::from_settings(settings)?;
        let tasks = selected_pages
            .iter()
            .map(|page_index| PageOcrTask {
                page_index: *page_index,
                pdf: extract_single_pdf_page(&request.source_path, *page_index),
            })
            .collect::<Vec<_>>();
        let next_task = AtomicUsize::new(0);
        let (sender, receiver) = sync_mpsc::channel::<PageOcrWorkerResult>();
        let worker_count = bounded_ocr_worker_count(tasks.len());
        let mut results = Vec::with_capacity(tasks.len());
        let mut ready_pages = native.pages.iter().cloned().map(Some).collect::<Vec<_>>();
        for page_index in &selected_pages {
            ready_pages[*page_index] = None;
        }
        let stream_metadata = DocumentMetadata {
            document_id: document_id.to_string(),
            source_path: request.source_path.clone(),
            provider: format!("native+{}-page-stream", client.provider_name()),
            provider_version: PIPELINE_VERSION.to_string(),
            extra: Default::default(),
        };
        let mut emitted_prefix = ready_pages.iter().take_while(|page| page.is_some()).count();
        let initial_prefix_document = (emitted_prefix > 0).then(|| {
            let mut document = DocumentV1::new(
                stream_metadata.clone(),
                ready_pages[..emitted_prefix]
                    .iter()
                    .map(|page| page.as_ref().expect("ready prefix contains a page").clone())
                    .collect(),
            );
            reconcile_provider_continuations(&mut document);
            document
        });
        if let Some(document) = &initial_prefix_document {
            for page in &document.pages {
                ready_pages[page.page_index] = Some(page.clone());
            }
        }
        for page_index in 0..emitted_prefix {
            update_sink(PdfTranslationSessionUpdate::PagePrepared {
                metadata: stream_metadata.clone(),
                page: ready_pages[page_index]
                    .as_ref()
                    .expect("ready prefix contains a page")
                    .clone(),
                page_count: ready_pages.len(),
                ready_prefix: emitted_prefix,
            });
        }
        std::thread::scope(|scope| {
            for _ in 0..worker_count {
                let sender = sender.clone();
                let client = client.clone();
                let native = &native;
                let tasks = &tasks;
                let next_task = &next_task;
                scope.spawn(move || {
                    loop {
                        let task_index = next_task.fetch_add(1, AtomicOrdering::Relaxed);
                        let Some(task) = tasks.get(task_index) else {
                            break;
                        };
                        let result = run_ocr_page(
                            &client,
                            task,
                            native,
                            target_pages,
                            &request.source_path,
                            document_id,
                            cancellation,
                        );
                        if sender.send(result).is_err() {
                            break;
                        }
                    }
                });
            }
            // Start translating a native-text prefix only after the OCR
            // workers are running. This keeps preparation and translation
            // overlapped even when Auto mode can immediately release the
            // document's leading pages.
            if let Some(document) = initial_prefix_document {
                translate_ready_prefix(
                    self,
                    document,
                    &request.source_path,
                    inspection,
                    preview_job_directory,
                    backend,
                    cancellation,
                    streamed_units,
                    streamed_preview_units,
                    streamed_preview_revisions,
                    update_sink,
                );
            }
            drop(sender);
            for (completed, result) in receiver.into_iter().enumerate() {
                progress.report(
                    PdfTranslationStage::Ocr,
                    format!(
                        "OCR page {} finished ({}/{})",
                        result.page_index + 1,
                        completed + 1,
                        tasks.len()
                    ),
                    Some((completed + 1) as u32),
                    Some(tasks.len() as u32),
                );
                let page_index = result.page_index;
                let page = result
                    .page
                    .clone()
                    .unwrap_or_else(|| native.pages[page_index].clone());
                ready_pages[page_index] = Some(page);
                if let Some(failure) = &result.failure {
                    update_sink(PdfTranslationSessionUpdate::PageFailed(failure.clone()));
                }
                let ready_prefix = ready_pages.iter().take_while(|page| page.is_some()).count();
                let reconciled_prefix = if ready_prefix > emitted_prefix {
                    let mut prefix_document = DocumentV1::new(
                        stream_metadata.clone(),
                        ready_pages[..ready_prefix]
                            .iter()
                            .map(|page| {
                                page.as_ref().expect("ready prefix contains a page").clone()
                            })
                            .collect(),
                    );
                    reconcile_provider_continuations(&mut prefix_document);
                    for page in &prefix_document.pages {
                        let page_index = page.page_index;
                        ready_pages[page_index] = Some(page.clone());
                    }
                    Some(prefix_document)
                } else {
                    None
                };
                for page_index in emitted_prefix..ready_prefix {
                    update_sink(PdfTranslationSessionUpdate::PagePrepared {
                        metadata: stream_metadata.clone(),
                        page: ready_pages[page_index]
                            .as_ref()
                            .expect("ready prefix contains a page")
                            .clone(),
                        page_count: ready_pages.len(),
                        ready_prefix,
                    });
                }
                if ready_prefix > emitted_prefix {
                    update_sink(PdfTranslationSessionUpdate::Reconciled {
                        ready_prefix,
                        invalidated_unit_ids: Vec::new(),
                        final_pass: false,
                    });
                }
                if let Some(prefix_document) = reconciled_prefix {
                    translate_ready_prefix(
                        self,
                        prefix_document,
                        &request.source_path,
                        inspection,
                        preview_job_directory,
                        backend,
                        cancellation,
                        streamed_units,
                        streamed_preview_units,
                        streamed_preview_revisions,
                        update_sink,
                    );
                }
                emitted_prefix = ready_prefix;
                results.push(result);
            }
        });
        cancellation
            .check()
            .map_err(|_| PdfTranslationError::cancelled(PdfTranslationStage::Ocr))?;

        let mut pages = native.pages.clone();
        let raw_directory = preparation_directory.join("ocr-pages");
        create_private_directory(&raw_directory).map_err(|error| {
            PdfTranslationError::stage(
                PdfTranslationStage::Ocr,
                "Could not create the per-page OCR artifact directory",
                error,
            )
        })?;
        for result in results {
            let page_index = result.page_index;
            if let Some(payload) = result.raw_payload {
                write_json(
                    raw_directory.join(format!("page-{:04}-raw.json", page_index + 1)),
                    &payload,
                )?;
            }
            if let Some(report) = result.fusion_report {
                write_json(
                    raw_directory.join(format!("page-{:04}-fusion.json", page_index + 1)),
                    &report,
                )?;
            }
            let mut page = result.page.unwrap_or_else(|| pages[page_index].clone());
            if let Some(failure) = result.failure {
                page.metadata.insert(
                    "ocr_failure".to_string(),
                    serde_json::json!({
                        "message": failure.message,
                        "retryable": failure.retryable,
                    }),
                );
            } else {
                page.metadata.remove("ocr_failure");
            }
            pages[page_index] = page;
        }
        let mut document = DocumentV1::new(stream_metadata, pages);
        reconcile_provider_continuations(&mut document);
        update_sink(PdfTranslationSessionUpdate::Reconciled {
            ready_prefix: document.pages.len(),
            invalidated_unit_ids: Vec::new(),
            final_pass: false,
        });
        document.validate().map_err(|error| {
            PdfTranslationError::stage(
                PdfTranslationStage::Normalizing,
                "Could not validate the page-streamed OCR document",
                error,
            )
        })?;
        progress.finish_stage(PdfTranslationStage::Ocr, "Per-page OCR completed");
        progress.finish_stage(
            PdfTranslationStage::Normalizing,
            "OCR pages fused with native PDF geometry",
        );
        Ok(document)
    }
}

const MAX_ACTIVE_OCR_PAGES: usize = 3;

fn bounded_ocr_worker_count(task_count: usize) -> usize {
    task_count.min(MAX_ACTIVE_OCR_PAGES)
}

#[derive(Clone)]
enum PageOcrClient {
    Paddle { client: PaddleClient, model: String },
    Unlimited(UnlimitedClient),
}

impl PageOcrClient {
    fn from_settings(settings: &Settings) -> Result<Self, PdfTranslationError> {
        match settings.ocr.provider {
            OcrProvider::Paddle => PaddleClient::new(
                PaddleClientConfig {
                    base_url: settings.ocr.paddle.endpoint.clone(),
                    ..PaddleClientConfig::default()
                },
                settings.ocr.paddle.token.clone(),
            )
            .map(|client| Self::Paddle {
                client,
                model: settings.ocr.paddle.model.clone(),
            })
            .map_err(|error| {
                PdfTranslationError::message(PdfTranslationStage::Ocr, error.to_string())
            }),
            OcrProvider::Unlimited => UnlimitedClient::new(
                UnlimitedClientConfig {
                    base_url: settings.ocr.unlimited.endpoint.clone(),
                    ..UnlimitedClientConfig::default()
                },
                settings.ocr.unlimited.api_key.clone(),
                settings.ocr.unlimited.secret_key.clone(),
            )
            .map(Self::Unlimited)
            .map_err(|error| {
                PdfTranslationError::message(PdfTranslationStage::Ocr, error.to_string())
            }),
        }
    }

    fn provider_name(&self) -> &'static str {
        match self {
            Self::Paddle { .. } => "paddle",
            Self::Unlimited(_) => "unlimited",
        }
    }
}

struct PageOcrTask {
    page_index: usize,
    pdf: Result<Vec<u8>, PdfTranslationError>,
}

struct PageOcrWorkerResult {
    page_index: usize,
    page: Option<Page>,
    raw_payload: Option<serde_json::Value>,
    fusion_report: Option<OcrFusionReport>,
    failure: Option<PdfTranslationPageFailure>,
}

fn run_ocr_page(
    client: &PageOcrClient,
    task: &PageOcrTask,
    native: &DocumentV1,
    target_pages: &[crate::PageGeometry],
    source_path: &Path,
    document_id: &str,
    cancellation: &CancellationToken,
) -> PageOcrWorkerResult {
    let failed = |message: String, retryable: bool| PageOcrWorkerResult {
        page_index: task.page_index,
        page: None,
        raw_payload: None,
        fusion_report: None,
        failure: Some(PdfTranslationPageFailure {
            page_index: task.page_index,
            phase: TranslationFailurePhase::Ocr,
            message,
            retryable,
        }),
    };
    let bytes = match &task.pdf {
        Ok(bytes) => bytes,
        Err(error) => return failed(error.message.clone(), error.retryable),
    };
    if cancellation.is_cancelled() {
        return failed("PDF translation was cancelled".to_string(), false);
    }
    let file_name = format!("page-{:04}.pdf", task.page_index + 1);
    let payload = match client {
        PageOcrClient::Paddle { client, model } => client
            .run_document_bytes(&file_name, bytes, model, cancellation, |_| {})
            .map(|result| result.payload)
            .map_err(|error| {
                (
                    error.to_string(),
                    matches!(
                        error.category,
                        crate::PaddleErrorCategory::QueueFull
                            | crate::PaddleErrorCategory::ServiceUnavailable
                            | crate::PaddleErrorCategory::PollTimeout
                    ),
                )
            }),
        PageOcrClient::Unlimited(client) => client
            .run_document_bytes(&file_name, bytes, cancellation, |_| {})
            .map(|result| result.payload)
            .map_err(|error| {
                (
                    error.to_string(),
                    matches!(
                        error.category,
                        crate::UnlimitedErrorCategory::QueueFull
                            | crate::UnlimitedErrorCategory::ServiceUnavailable
                            | crate::UnlimitedErrorCategory::PollTimeout
                    ),
                )
            }),
    };
    let payload = match payload {
        Ok(payload) => payload,
        Err((message, retryable)) => return failed(message, retryable),
    };
    let Some(geometry) = target_pages.get(task.page_index).copied() else {
        return failed(
            format!("page {} has no source geometry", task.page_index + 1),
            false,
        );
    };
    let normalized = match client {
        PageOcrClient::Paddle { model, .. } => {
            normalize_paddle_payload(&payload, source_path, document_id, model, &[geometry])
        }
        PageOcrClient::Unlimited(_) => {
            normalize_unlimited_payload(&payload, source_path, document_id, &[geometry])
        }
    };
    let mut normalized = match normalized {
        Ok(document) => document,
        Err(error) => {
            return PageOcrWorkerResult {
                raw_payload: Some(payload),
                ..failed(format!("Could not normalize OCR page: {error}"), false)
            };
        }
    };
    let Some(page) = normalized.pages.pop() else {
        return PageOcrWorkerResult {
            raw_payload: Some(payload),
            ..failed("OCR result did not contain a page".to_string(), false)
        };
    };
    let page = globalize_ocr_page(page, task.page_index);
    let ocr_document = DocumentV1::new(normalized.metadata, vec![page]);
    let Some(native_page) = native.pages.get(task.page_index).cloned() else {
        return PageOcrWorkerResult {
            raw_payload: Some(payload),
            ..failed("Native fallback page is unavailable".to_string(), false)
        };
    };
    let native_document = DocumentV1::new(native.metadata.clone(), vec![native_page]);
    let fusion = fuse_native_with_ocr_report(native_document, &ocr_document);
    PageOcrWorkerResult {
        page_index: task.page_index,
        page: fusion.document.pages.into_iter().next(),
        raw_payload: Some(payload),
        fusion_report: Some(fusion.report),
        failure: None,
    }
}

fn globalize_ocr_page(mut page: Page, page_index: usize) -> Page {
    let page_prefix = format!("p{:03}-", page_index + 1);
    let replace_local_page_id = |value: &str| {
        value.replace("p001-", &page_prefix).replace(
            "provider-paddle-page-001-",
            &format!("provider-paddle-page-{:03}-", page_index + 1),
        )
    };
    page.page_index = page_index;
    for block in &mut page.blocks {
        block.page_index = page_index;
        block.id = replace_local_page_id(&block.id);
        if let Some(continuation) = &mut block.continuation {
            continuation.group_id = replace_local_page_id(&continuation.group_id);
        }
        if let Some(structure) = &mut block.structure
            && let Some(group_id) = &mut structure.group_id
        {
            *group_id = replace_local_page_id(group_id);
        }
    }
    for region in &mut page.protected_regions {
        region.page_index = page_index;
        region.id = replace_local_page_id(&region.id);
    }
    for object in &mut page.inline_objects {
        object.page_index = page_index;
        object.id = replace_local_page_id(&object.id);
    }
    page
}

fn translate_ready_prefix(
    service: &PdfTranslationService,
    mut document: DocumentV1,
    source_path: &Path,
    inspection: &PdfInspection,
    job_directory: &Path,
    backend: &GlyphstTranslationBackend,
    cancellation: &CancellationToken,
    streamed_units: &mut Vec<TranslatedUnit>,
    known_preview_units: &mut HashMap<String, (u64, Vec<usize>)>,
    preview_revisions: &mut HashMap<usize, u64>,
    update_sink: &mut impl FnMut(PdfTranslationSessionUpdate),
) {
    if cancellation.is_cancelled() || document.pages.is_empty() {
        return;
    }
    annotate_reference_sections(&mut document);
    let resumed = streamed_units.clone();
    match translate_document_partial_resuming(
        document,
        backend,
        cancellation,
        resumed,
        |snapshot| {
            update_sink(PdfTranslationSessionUpdate::UnitsChanged(snapshot.clone()));
            Ok(())
        },
    ) {
        Ok(outcome) => {
            let block_pages = translation_block_locations(&outcome.document.document)
                .into_iter()
                .map(|(block_id, location)| (block_id, location.page_index))
                .collect::<HashMap<_, _>>();
            let (next_preview_units, dirty_pages) =
                reconcile_preview_units(&block_pages, known_preview_units, &outcome.document.units);
            *known_preview_units = next_preview_units;
            for page_index in dirty_pages {
                let revision = preview_revisions.entry(page_index).or_default();
                *revision = revision.wrapping_add(1).max(1);
                let update = match service.compile_page_preview(
                    &outcome.document,
                    source_path,
                    inspection,
                    job_directory,
                    page_index,
                    *revision,
                    &HashSet::new(),
                    cancellation,
                ) {
                    Ok(preview) => PdfTranslationSessionUpdate::PagePreviewReady(preview),
                    Err(error) => {
                        PdfTranslationSessionUpdate::PageFailed(PdfTranslationPageFailure {
                            page_index,
                            phase: if error.stage == PdfTranslationStage::Layout {
                                TranslationFailurePhase::Layout
                            } else {
                                TranslationFailurePhase::Preview
                            },
                            message: error.message,
                            retryable: error.retryable,
                        })
                    }
                };
                update_sink(update);
            }
            *streamed_units = outcome.document.units;
        }
        Err(_error) if cancellation.is_cancelled() => {}
        Err(error) => {
            // The final document-wide pass retries unresolved work after OCR
            // reconciliation, so a provisional prefix error is non-terminal.
            eprintln!(
                "incremental PDF translation prefix failed: {}",
                error.message
            );
        }
    }
}

fn ocr_page_failures(document: &DocumentV1) -> Vec<PdfTranslationPageFailure> {
    document
        .pages
        .iter()
        .filter_map(|page| {
            let failure = page.metadata.get("ocr_failure")?;
            Some(PdfTranslationPageFailure {
                page_index: page.page_index,
                phase: TranslationFailurePhase::Ocr,
                message: failure
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("OCR failed; the source page was preserved")
                    .to_string(),
                retryable: failure
                    .get("retryable")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

fn ocr_failure_diagnostics(document: &DocumentV1) -> Vec<TranslationFailureDiagnostic> {
    ocr_page_failures(document)
        .into_iter()
        .map(|failure| {
            let page = &document.pages[failure.page_index];
            TranslationFailureDiagnostic {
                id: format!("ocr-page-{:04}", failure.page_index + 1),
                phase: TranslationFailurePhase::Ocr,
                reason: failure.message,
                attempts: 1,
                retryable: failure.retryable,
                category: Some("ocr".to_string()),
                status: None,
                member_block_ids: page.blocks.iter().map(|block| block.id.clone()).collect(),
                role: None,
                source_excerpt: page.blocks.iter().find_map(|block| {
                    (!block.content.text.trim().is_empty())
                        .then(|| block.content.text.chars().take(480).collect::<String>())
                }),
                rejected_output_excerpt: None,
            }
        })
        .collect()
}

const PAGE_PREVIEW_DEBOUNCE: Duration = Duration::from_millis(120);
const PAGE_PREVIEW_MAX_DELAY: Duration = Duration::from_millis(250);

type PreviewUnitIdentity = (u64, Vec<usize>);

fn reconcile_preview_units(
    block_pages: &HashMap<String, usize>,
    known_units: &HashMap<String, PreviewUnitIdentity>,
    resolved_units: &[TranslatedUnit],
) -> (HashMap<String, PreviewUnitIdentity>, BTreeSet<usize>) {
    let mut next_units = HashMap::new();
    let mut dirty_pages = BTreeSet::new();
    for unit in resolved_units {
        let mut pages = unit
            .member_block_ids
            .iter()
            .filter_map(|block_id| block_pages.get(block_id).copied())
            .collect::<Vec<_>>();
        pages.sort_unstable();
        pages.dedup();
        let mut hasher = DefaultHasher::new();
        unit.semantic_hash.hash(&mut hasher);
        unit.translated_text.hash(&mut hasher);
        unit.styled_runs
            .iter()
            .for_each(|run| run.text.hash(&mut hasher));
        let identity = hasher.finish();
        if known_units
            .get(&unit.id)
            .is_none_or(|known| known.0 != identity || known.1 != pages)
        {
            dirty_pages.extend(pages.iter().copied());
            if let Some((_, old_pages)) = known_units.get(&unit.id) {
                dirty_pages.extend(old_pages.iter().copied());
            }
        }
        next_units.insert(unit.id.clone(), (identity, pages));
    }
    for (unit_id, (_, pages)) in known_units {
        if !next_units.contains_key(unit_id) {
            dirty_pages.extend(pages.iter().copied());
        }
    }
    (next_units, dirty_pages)
}

fn stream_page_previews<U>(
    service: &PdfTranslationService,
    document: &DocumentV1,
    source_path: &Path,
    inspection: &PdfInspection,
    job_directory: &Path,
    cancellation: &CancellationToken,
    mut known_units: HashMap<String, (u64, Vec<usize>)>,
    mut revisions: HashMap<usize, u64>,
    receiver: sync_mpsc::Receiver<TranslationRunSnapshot>,
    update_sink: &Mutex<&mut U>,
) where
    U: FnMut(PdfTranslationSessionUpdate) + Send,
{
    let block_pages = translation_block_locations(document)
        .into_iter()
        .map(|(block_id, location)| (block_id, location.page_index))
        .collect::<HashMap<_, _>>();
    let mut latest = None::<TranslationRunSnapshot>;
    let mut dirty_pages = BTreeSet::<usize>::new();
    let mut first_dirty_at = None::<Instant>;
    let mut last_change_at = None::<Instant>;

    loop {
        let wait = match (first_dirty_at, last_change_at) {
            (Some(first), Some(last)) => {
                let now = Instant::now();
                let debounce_deadline = last.checked_add(PAGE_PREVIEW_DEBOUNCE).unwrap_or(last);
                let maximum_deadline = first.checked_add(PAGE_PREVIEW_MAX_DELAY).unwrap_or(first);
                debounce_deadline
                    .min(maximum_deadline)
                    .saturating_duration_since(now)
            }
            _ => Duration::from_secs(60),
        };
        let received = receiver.recv_timeout(wait);
        match received {
            Ok(snapshot) => {
                let (next_units, changed_pages) =
                    reconcile_preview_units(&block_pages, &known_units, &snapshot.resolved_units);
                dirty_pages.extend(changed_pages);
                known_units = next_units;
                latest = Some(snapshot);
                if !dirty_pages.is_empty() {
                    let now = Instant::now();
                    first_dirty_at.get_or_insert(now);
                    last_change_at = Some(now);
                }
            }
            Err(sync_mpsc::RecvTimeoutError::Timeout) => {
                compile_dirty_page_previews(
                    service,
                    document,
                    source_path,
                    inspection,
                    job_directory,
                    cancellation,
                    latest.as_ref(),
                    &mut dirty_pages,
                    &mut revisions,
                    update_sink,
                );
                first_dirty_at = None;
                last_change_at = None;
            }
            Err(sync_mpsc::RecvTimeoutError::Disconnected) => {
                compile_dirty_page_previews(
                    service,
                    document,
                    source_path,
                    inspection,
                    job_directory,
                    cancellation,
                    latest.as_ref(),
                    &mut dirty_pages,
                    &mut revisions,
                    update_sink,
                );
                break;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compile_dirty_page_previews<U>(
    service: &PdfTranslationService,
    document: &DocumentV1,
    source_path: &Path,
    inspection: &PdfInspection,
    job_directory: &Path,
    cancellation: &CancellationToken,
    snapshot: Option<&TranslationRunSnapshot>,
    dirty_pages: &mut BTreeSet<usize>,
    revisions: &mut HashMap<usize, u64>,
    update_sink: &Mutex<&mut U>,
) where
    U: FnMut(PdfTranslationSessionUpdate) + Send,
{
    let Some(snapshot) = snapshot else {
        dirty_pages.clear();
        return;
    };
    let translated = TranslatedDocument {
        document: document.clone(),
        units: snapshot.resolved_units.clone(),
    };
    let pages = std::mem::take(dirty_pages);
    for page_index in pages {
        if cancellation.is_cancelled() {
            break;
        }
        let revision = revisions.entry(page_index).or_default();
        *revision = revision.wrapping_add(1).max(1);
        let update = match service.compile_page_preview(
            &translated,
            source_path,
            inspection,
            job_directory,
            page_index,
            *revision,
            &HashSet::new(),
            cancellation,
        ) {
            Ok(preview) => PdfTranslationSessionUpdate::PagePreviewReady(preview),
            Err(error) => PdfTranslationSessionUpdate::PageFailed(PdfTranslationPageFailure {
                page_index,
                phase: if error.stage == PdfTranslationStage::Layout {
                    TranslationFailurePhase::Layout
                } else {
                    TranslationFailurePhase::Preview
                },
                message: error.message,
                retryable: error.retryable,
            }),
        };
        let Ok(mut sink) = update_sink.lock() else {
            break;
        };
        (*sink)(update);
    }
}

#[derive(Debug, Clone)]
pub struct PdfTranslationRequest {
    pub source_path: PathBuf,
    pub output_path: PathBuf,
    pub source_language: Option<LanguageCode>,
    pub target_language: LanguageCode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfTranslationSessionRequest {
    pub source_path: PathBuf,
    pub source_language: Option<LanguageCode>,
    pub target_language: LanguageCode,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PdfTranslationSessionUpdate {
    PagePrepared {
        metadata: DocumentMetadata,
        page: Page,
        page_count: usize,
        ready_prefix: usize,
    },
    UnitsChanged(TranslationRunSnapshot),
    PagePreviewReady(PdfTranslationPagePreview),
    PageFailed(PdfTranslationPageFailure),
    Reconciled {
        ready_prefix: usize,
        invalidated_unit_ids: Vec<String>,
        final_pass: bool,
    },
    Finished,
}

fn build_page_hover_regions(
    translated: &TranslatedDocument,
    layout_page: &LayoutPage,
) -> Vec<PdfTranslationHoverRegion> {
    let Some(source_page) = translated
        .document
        .pages
        .iter()
        .find(|page| page.page_index == layout_page.page_index)
    else {
        return layout_page
            .items
            .iter()
            .map(|item| PdfTranslationHoverRegion {
                id: hover_region_id(layout_page.page_index, "block", &item.block_id),
                kind: PdfTranslationHoverRegionKind::Text,
                unit_id: Some(item.unit_id.clone()),
                block_id: Some(item.block_id.clone()),
                target_bounds: item.bbox,
                source_regions: if item.source_regions.is_empty() {
                    vec![item.source_bbox]
                } else {
                    item.source_regions.clone()
                },
            })
            .collect();
    };

    let layout_items = layout_page
        .items
        .iter()
        .map(|item| (item.block_id.as_str(), item))
        .collect::<HashMap<_, _>>();
    let unit_ids = translated
        .units
        .iter()
        .flat_map(|unit| {
            unit.member_block_ids
                .iter()
                .map(move |block_id| (block_id.as_str(), unit.id.as_str()))
        })
        .collect::<HashMap<_, _>>();
    let mut represented_blocks = HashSet::new();
    let mut regions = Vec::new();

    for block in &source_page.blocks {
        if block.kind != BlockKind::Text
            && source_page.protected_regions.iter().any(|region| {
                region
                    .bbox
                    .intersection(block.bbox)
                    .is_some_and(|intersection| {
                        intersection.area() / region.bbox.area().min(block.bbox.area()).max(1.0)
                            >= 0.8
                    })
            })
        {
            continue;
        }
        represented_blocks.insert(block.id.as_str());
        let layout_item = layout_items.get(block.id.as_str()).copied();
        let source_regions = layout_item
            .filter(|item| !item.source_regions.is_empty())
            .map(|item| item.source_regions.clone())
            .unwrap_or_else(|| source_text_regions(block));
        regions.push(PdfTranslationHoverRegion {
            id: hover_region_id(layout_page.page_index, "block", &block.id),
            kind: hover_region_kind_for_block(block.kind),
            unit_id: layout_item
                .map(|item| item.unit_id.clone())
                .or_else(|| unit_ids.get(block.id.as_str()).map(|id| (*id).to_string())),
            block_id: Some(block.id.clone()),
            target_bounds: layout_item.map_or(block.bbox, |item| item.bbox),
            source_regions,
        });
    }

    // A malformed provider response should not make a successfully rendered
    // layout item impossible to target. Keep these as a conservative fallback.
    for item in &layout_page.items {
        if represented_blocks.contains(item.block_id.as_str()) {
            continue;
        }
        regions.push(PdfTranslationHoverRegion {
            id: hover_region_id(layout_page.page_index, "block", &item.block_id),
            kind: PdfTranslationHoverRegionKind::Text,
            unit_id: Some(item.unit_id.clone()),
            block_id: Some(item.block_id.clone()),
            target_bounds: item.bbox,
            source_regions: if item.source_regions.is_empty() {
                vec![item.source_bbox]
            } else {
                item.source_regions.clone()
            },
        });
    }

    regions.extend(
        source_page
            .protected_regions
            .iter()
            .map(|region| PdfTranslationHoverRegion {
                id: hover_region_id(layout_page.page_index, "protected", &region.id),
                kind: hover_region_kind_for_protected(region.kind),
                unit_id: None,
                block_id: None,
                target_bounds: region.bbox,
                source_regions: vec![region.bbox],
            }),
    );
    regions.sort_by(|left, right| left.id.cmp(&right.id));
    regions
}

fn hover_region_id(page_index: usize, namespace: &str, source_id: &str) -> String {
    format!("page:{page_index}:{namespace}:{source_id}")
}

fn hover_region_kind_for_block(kind: BlockKind) -> PdfTranslationHoverRegionKind {
    match kind {
        BlockKind::Text => PdfTranslationHoverRegionKind::Text,
        BlockKind::Image => PdfTranslationHoverRegionKind::Image,
        BlockKind::Table => PdfTranslationHoverRegionKind::Table,
        BlockKind::Formula => PdfTranslationHoverRegionKind::Formula,
        BlockKind::Code => PdfTranslationHoverRegionKind::Code,
        BlockKind::Unknown => PdfTranslationHoverRegionKind::Unknown,
    }
}

fn hover_region_kind_for_protected(kind: ProtectedRegionKind) -> PdfTranslationHoverRegionKind {
    match kind {
        ProtectedRegionKind::Chart => PdfTranslationHoverRegionKind::Chart,
        ProtectedRegionKind::Table => PdfTranslationHoverRegionKind::Table,
        ProtectedRegionKind::Image => PdfTranslationHoverRegionKind::Image,
        ProtectedRegionKind::Formula => PdfTranslationHoverRegionKind::Formula,
        ProtectedRegionKind::Code => PdfTranslationHoverRegionKind::Code,
        ProtectedRegionKind::Drawing => PdfTranslationHoverRegionKind::Drawing,
        ProtectedRegionKind::Unknown => PdfTranslationHoverRegionKind::Unknown,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PdfTranslationPagePreview {
    pub page_index: usize,
    pub revision: u64,
    pub layout: LayoutPage,
    pub hover_regions: Vec<PdfTranslationHoverRegion>,
    pub pdf: Arc<[u8]>,
    pub layout_failures: Vec<TranslationFailureDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfTranslationHoverRegionKind {
    Text,
    Image,
    Chart,
    Table,
    Formula,
    Code,
    Drawing,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PdfTranslationHoverRegion {
    pub id: String,
    pub kind: PdfTranslationHoverRegionKind,
    pub unit_id: Option<String>,
    pub block_id: Option<String>,
    pub target_bounds: BoundingBox,
    pub source_regions: Vec<BoundingBox>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfTranslationPageFailure {
    pub page_index: usize,
    pub phase: TranslationFailurePhase,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PdfTranslationSessionResult {
    pub request: PdfTranslationSessionRequest,
    pub translated: TranslatedDocument,
    pub failures: Vec<TranslationFailureDiagnostic>,
    pub progress: TranslationRunProgress,
    pub queue: TranslationQueueStats,
    pub inspection: PdfInspection,
    pub source_snapshot_path: PathBuf,
    pub job_directory: PathBuf,
    pub fingerprint: String,
    pub warnings: Vec<PdfTranslationWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfTranslationExportRequest {
    pub output_path: PathBuf,
    pub original_unit_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfTranslationResult {
    pub output_path: PathBuf,
    pub manifest_path: PathBuf,
    pub job_directory: PathBuf,
    pub fingerprint: String,
    pub warnings: Vec<PdfTranslationWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfTranslationWarning {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_block_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfTranslationError {
    pub stage: PdfTranslationStage,
    pub message: String,
    pub retryable: bool,
    pub detailed_message: Option<String>,
}

impl PdfTranslationError {
    fn cancelled(stage: PdfTranslationStage) -> Self {
        Self {
            stage,
            message: "PDF translation was cancelled".to_string(),
            retryable: false,
            detailed_message: None,
        }
    }

    fn message(stage: PdfTranslationStage, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
            retryable: false,
            detailed_message: None,
        }
    }

    fn stage(stage: PdfTranslationStage, context: &str, error: impl fmt::Display) -> Self {
        Self {
            stage,
            message: format!("{context}: {error}"),
            retryable: false,
            detailed_message: None,
        }
    }
}

impl fmt::Display for PdfTranslationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for PdfTranslationError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TranslationCheckpointIdentity {
    pipeline_version: String,
    fingerprint: String,
    document_id: String,
    source_language: Option<String>,
    target_language: String,
    provider_identity: String,
}

impl TranslationCheckpointIdentity {
    fn new(
        settings: &Settings,
        request: &PdfTranslationRequest,
        fingerprint: &str,
        document: &DocumentV1,
    ) -> Self {
        Self {
            pipeline_version: PIPELINE_VERSION.to_string(),
            fingerprint: fingerprint.to_string(),
            document_id: document.metadata.document_id.clone(),
            source_language: request
                .source_language
                .as_ref()
                .map(|language| language.as_str().to_string()),
            target_language: request.target_language.as_str().to_string(),
            provider_identity: translation_provider_identity(settings),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct TranslationCheckpoint {
    version: u32,
    identity: TranslationCheckpointIdentity,
    resolved_units: Vec<TranslatedUnit>,
    failures: Vec<TranslationFailureDiagnostic>,
    progress: TranslationRunProgress,
    queue: TranslationQueueStats,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct TranslationDiagnostics {
    version: u32,
    identity: TranslationCheckpointIdentity,
    failures: Vec<TranslationFailureDiagnostic>,
    progress: TranslationRunProgress,
    queue: TranslationQueueStats,
}

#[derive(Debug, Clone, Copy)]
struct TranslationBlockLocation {
    page_index: usize,
    bbox: BoundingBox,
}

fn translation_block_locations(document: &DocumentV1) -> HashMap<String, TranslationBlockLocation> {
    document
        .pages
        .iter()
        .flat_map(|page| {
            page.blocks.iter().map(|block| {
                (
                    block.id.clone(),
                    TranslationBlockLocation {
                        page_index: page.page_index,
                        bbox: block.bbox,
                    },
                )
            })
        })
        .collect()
}

fn translation_failure_report(
    request: &PdfTranslationRequest,
    fingerprint: &str,
    job_directory: &Path,
    diagnostics_path: &Path,
    identity: &TranslationCheckpointIdentity,
    block_locations: &HashMap<String, TranslationBlockLocation>,
    snapshot: Option<&TranslationRunSnapshot>,
    error: &TranslationDocumentError,
) -> String {
    let persisted_diagnostics = snapshot.is_none().then(|| {
        fs::read(diagnostics_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<TranslationDiagnostics>(&bytes).ok())
    });
    let (failures, progress, queue) = if let Some(snapshot) = snapshot {
        (
            snapshot.failures.clone(),
            snapshot.progress.clone(),
            snapshot.queue.clone(),
        )
    } else if let Some(diagnostics) = persisted_diagnostics.flatten() {
        (
            diagnostics.failures,
            diagnostics.progress,
            diagnostics.queue,
        )
    } else {
        (
            Vec::new(),
            TranslationRunProgress::default(),
            TranslationQueueStats::default(),
        )
    };

    let mut report = String::new();
    let _ = writeln!(report, "Glyphst PDF translation failure");
    let _ = writeln!(report);
    let _ = writeln!(report, "Summary: {}", error.message);
    let _ = writeln!(report, "Stage: translating");
    let _ = writeln!(report, "Retryable: {}", error.retryable);
    let _ = writeln!(report, "Source PDF: {}", request.source_path.display());
    let _ = writeln!(report, "Output PDF: {}", request.output_path.display());
    let _ = writeln!(
        report,
        "Languages: {} -> {}",
        request
            .source_language
            .as_ref()
            .map(LanguageCode::as_str)
            .unwrap_or("auto"),
        request.target_language.as_str()
    );
    let _ = writeln!(
        report,
        "Translation provider: {}",
        diagnostic_provider_identity(&identity.provider_identity)
    );
    let _ = writeln!(report, "Pipeline: {}", identity.pipeline_version);
    let _ = writeln!(report, "Fingerprint: {fingerprint}");
    let _ = writeln!(report, "Job directory: {}", job_directory.display());
    let _ = writeln!(report, "Diagnostics: {}", diagnostics_path.display());
    let _ = writeln!(
        report,
        "Progress: translated={}, cached={}, retrying={}, failed={}, total={}",
        progress.translated, progress.cached, progress.retrying, progress.failed, progress.total
    );
    let _ = writeln!(
        report,
        "Requests: planned={}, dispatched={}, completed={}, retries={} \
         (provider={}, validation={}), peak_active={}, elapsed_ms={}",
        queue.planned_requests,
        queue.dispatched_requests,
        queue.completed_requests,
        queue.retry_requests,
        queue.provider_retry_requests,
        queue.validation_retry_requests,
        queue.peak_active_requests,
        queue.elapsed_ms
    );
    let _ = writeln!(report);
    let _ = writeln!(
        report,
        "Unresolved translation units ({})",
        error.unresolved_unit_ids.len()
    );

    for (index, unit_id) in error.unresolved_unit_ids.iter().enumerate() {
        let _ = writeln!(report);
        let _ = writeln!(report, "{}. {unit_id}", index + 1);
        let Some(failure) = failures.iter().find(|failure| failure.id == *unit_id) else {
            let _ = writeln!(report, "   Diagnostic: unavailable");
            continue;
        };
        let _ = writeln!(
            report,
            "   Role: {}",
            failure.role.map_or("unknown", |role| role.as_str())
        );
        let _ = writeln!(report, "   Attempts: {}", failure.attempts);
        let _ = writeln!(report, "   Retryable: {}", failure.retryable);
        if let Some(category) = &failure.category {
            let _ = writeln!(report, "   Category: {category}");
        }
        if let Some(status) = failure.status {
            let _ = writeln!(report, "   Provider status: {status}");
        }
        let _ = writeln!(report, "   Reason: {}", failure.reason);

        let legacy_block_id = unit_id.strip_prefix("unit-");
        let block_ids = if failure.member_block_ids.is_empty() {
            legacy_block_id.into_iter().collect::<Vec<_>>()
        } else {
            failure
                .member_block_ids
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        };
        if block_ids.is_empty() {
            let _ = writeln!(report, "   Location: unavailable");
        }
        for block_id in block_ids {
            if let Some(location) = block_locations.get(block_id) {
                let bbox = location.bbox;
                let _ = writeln!(
                    report,
                    "   Location: page {}, block {}, bbox ({:.2}, {:.2})-({:.2}, {:.2})",
                    location.page_index + 1,
                    block_id,
                    bbox.x0,
                    bbox.y0,
                    bbox.x1,
                    bbox.y1
                );
            } else {
                let _ = writeln!(report, "   Location: block {block_id} (page unavailable)");
            }
        }
        if let Some(excerpt) = &failure.source_excerpt {
            let _ = writeln!(report, "   Source excerpt: {excerpt}");
        }
        if let Some(excerpt) = &failure.rejected_output_excerpt {
            let _ = writeln!(report, "   Rejected output excerpt: {excerpt}");
        }
    }

    report
}

fn diagnostic_provider_identity(identity: &str) -> String {
    identity
        .split('|')
        .map(|part| {
            if part.contains("://") {
                redact_endpoint_credentials(part)
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn redact_endpoint_credentials(endpoint: &str) -> String {
    let endpoint = endpoint
        .split_once(['?', '#'])
        .map_or(endpoint, |(endpoint, _)| endpoint);
    let Some((scheme, remainder)) = endpoint.split_once("://") else {
        return endpoint.to_string();
    };
    let remainder = remainder
        .split_once('@')
        .map_or(remainder, |(_, host_and_path)| host_and_path);
    format!("{scheme}://{remainder}")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfTranslationArtifactManifest {
    pub version: u32,
    pub pipeline_version: String,
    pub fingerprint: String,
    pub source_path: String,
    pub output_path: String,
    pub target_language: String,
    pub ocr_mode: String,
    pub ocr_provider: String,
    pub translation_provider: String,
    pub translated_unit_count: usize,
    #[serde(default)]
    pub preserved_on_overflow: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<PdfTranslationWarning>,
    #[serde(default)]
    pub failed_units: Vec<TranslationFailureDiagnostic>,
    #[serde(default)]
    pub original_unit_ids: Vec<String>,
    pub job_directory: String,
    pub geometry: Vec<ArtifactGeometry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactGeometry {
    pub block_id: String,
    pub page_index: usize,
    pub source_bbox: BoundingBox,
    pub target_bbox: BoundingBox,
}

fn build_manifest(
    settings: &Settings,
    request: &PdfTranslationRequest,
    fingerprint: &str,
    job_directory: &Path,
    layout: &LayoutDocument,
    translated: &TranslatedDocument,
    warnings: &[PdfTranslationWarning],
    failed_units: &[TranslationFailureDiagnostic],
    original_unit_ids: &[String],
) -> PdfTranslationArtifactManifest {
    PdfTranslationArtifactManifest {
        version: ARTIFACT_VERSION,
        pipeline_version: PIPELINE_VERSION.to_string(),
        fingerprint: fingerprint.to_string(),
        source_path: request.source_path.to_string_lossy().to_string(),
        output_path: request.output_path.to_string_lossy().to_string(),
        target_language: request.target_language.as_str().to_string(),
        ocr_mode: settings.ocr.mode.as_str().to_string(),
        ocr_provider: if translated.document.metadata.provider.contains("unlimited") {
            "unlimited".to_string()
        } else if translated.document.metadata.provider.contains("paddle") {
            "paddle".to_string()
        } else {
            "mupdf".to_string()
        },
        translation_provider: settings.translation.active_provider.as_str().to_string(),
        translated_unit_count: translated.units.len(),
        preserved_on_overflow: layout.preserved_on_overflow.clone(),
        warnings: warnings.to_vec(),
        failed_units: failed_units.to_vec(),
        original_unit_ids: original_unit_ids.to_vec(),
        job_directory: job_directory.to_string_lossy().to_string(),
        geometry: layout
            .pages
            .iter()
            .flat_map(|page| {
                page.items.iter().map(|item| ArtifactGeometry {
                    block_id: item.block_id.clone(),
                    page_index: item.page_index,
                    source_bbox: item.source_bbox,
                    target_bbox: item.bbox,
                })
            })
            .collect(),
    }
}

fn validate_rendered_output(
    source_path: &Path,
    output_path: &Path,
    layout: &LayoutDocument,
) -> Result<(), PdfTranslationError> {
    let source = Document::open(source_path).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Validating,
            "Could not reopen the source PDF",
            error,
        )
    })?;
    let output = Document::open(output_path).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Validating,
            "Could not open the translated PDF",
            error,
        )
    })?;
    let source_pages = source.page_count().map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Validating,
            "Could not count source pages",
            error,
        )
    })?;
    let output_pages = output.page_count().map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Validating,
            "Could not count translated pages",
            error,
        )
    })?;
    if source_pages != output_pages || output_pages as usize != layout.pages.len() {
        return Err(PdfTranslationError::message(
            PdfTranslationStage::Validating,
            "Translated PDF page count does not match the source",
        ));
    }
    let mut extracted_pages = Vec::with_capacity(output_pages as usize);
    for page_index in 0..output_pages {
        let page = output.load_page(page_index).map_err(|error| {
            PdfTranslationError::stage(
                PdfTranslationStage::Validating,
                "Could not load a translated page",
                error,
            )
        })?;
        let text = page
            .to_text_page(TextPageFlags::PRESERVE_WHITESPACE)
            .and_then(|page| page.to_text())
            .map_err(|error| {
                PdfTranslationError::stage(
                    PdfTranslationStage::Validating,
                    "Could not extract translated text",
                    error,
                )
            })?;
        extracted_pages.push(normalize_searchable_text(&text));
    }
    for page in &layout.pages {
        let Some(extracted) = extracted_pages.get(page.page_index) else {
            continue;
        };
        for item in &page.items {
            let expected = normalize_searchable_text(&item.text);
            if expected.chars().count() < 2 || extracted.contains(&expected) {
                continue;
            }
            let preview = expected.chars().take(48).collect::<String>();
            return Err(PdfTranslationError::message(
                PdfTranslationStage::Validating,
                format!(
                    "Translated PDF is missing searchable content for block {} ({preview})",
                    item.block_id
                ),
            ));
        }
    }
    Ok(())
}

fn validate_layout_completeness(
    translated: &TranslatedDocument,
    layout: &LayoutDocument,
) -> Result<(), PdfTranslationError> {
    if let Some(warning) = layout.warnings.iter().find(|warning| warning.fatal) {
        return Err(PdfTranslationError::message(
            PdfTranslationStage::Layout,
            format!(
                "Unsafe translated layout for {}: {}",
                warning.unit_id, warning.message
            ),
        ));
    }
    if !layout.preserved_on_overflow.is_empty() {
        return Err(PdfTranslationError::message(
            PdfTranslationStage::Layout,
            "Translated layout attempted to preserve source text after overflow",
        ));
    }
    let items = layout
        .pages
        .iter()
        .flat_map(|page| page.items.iter())
        .map(|item| (item.block_id.as_str(), item))
        .collect::<HashMap<_, _>>();
    for unit in &translated.units {
        let rendered = unit
            .member_block_ids
            .iter()
            .filter_map(|block_id| items.get(block_id.as_str()))
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let expected = normalize_layout_text(&unit.translated_text);
        let rendered = normalize_layout_text(&rendered);
        if expected.is_empty() || expected != rendered {
            return Err(PdfTranslationError::message(
                PdfTranslationStage::Layout,
                format!(
                    "Translated layout does not contain the complete text for unit {}",
                    unit.id
                ),
            ));
        }
    }
    Ok(())
}

fn layout_fallback_unit_ids(
    translated: &TranslatedDocument,
    layout: &LayoutDocument,
) -> Vec<String> {
    let mut fallback = layout
        .warnings
        .iter()
        .filter(|warning| warning.fatal)
        .map(|warning| warning.unit_id.clone())
        .collect::<HashSet<_>>();
    let items = layout
        .pages
        .iter()
        .flat_map(|page| page.items.iter())
        .map(|item| (item.block_id.as_str(), item))
        .collect::<HashMap<_, _>>();
    for unit in &translated.units {
        let rendered = unit
            .member_block_ids
            .iter()
            .filter_map(|block_id| items.get(block_id.as_str()))
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let expected = normalize_layout_text(&unit.translated_text);
        if expected.is_empty() || expected != normalize_layout_text(&rendered) {
            fallback.insert(unit.id.clone());
        }
    }
    let mut fallback = fallback.into_iter().collect::<Vec<_>>();
    fallback.sort();
    fallback
}

struct PreparedTranslationLayout {
    translated: TranslatedDocument,
    layout: LayoutDocument,
    failures: Vec<TranslationFailureDiagnostic>,
}

fn prepare_translation_layout(
    translated: &TranslatedDocument,
    original_unit_ids: &HashSet<String>,
) -> PreparedTranslationLayout {
    let mut translated = translated.clone();
    translated
        .units
        .retain(|unit| !original_unit_ids.contains(&unit.id));
    let mut failures = Vec::new();
    let mut layout = build_layout_with_options(&translated, LayoutOptions::application_default());
    loop {
        let fallback_ids = layout_fallback_unit_ids(&translated, &layout);
        if fallback_ids.is_empty() {
            break;
        }
        let fallback_ids = fallback_ids.into_iter().collect::<HashSet<_>>();
        let removed = translated
            .units
            .iter()
            .filter(|unit| fallback_ids.contains(&unit.id))
            .cloned()
            .collect::<Vec<_>>();
        if removed.is_empty() {
            break;
        }
        for unit in &removed {
            let reason = layout
                .warnings
                .iter()
                .find(|warning| warning.unit_id == unit.id)
                .map(|warning| warning.message.clone())
                .unwrap_or_else(|| {
                    "translated text could not be represented safely by the canonical layout"
                        .to_string()
                });
            failures.push(TranslationFailureDiagnostic {
                id: unit.id.clone(),
                phase: TranslationFailurePhase::Layout,
                reason,
                attempts: 1,
                retryable: false,
                category: Some("layout".to_string()),
                status: None,
                member_block_ids: unit.member_block_ids.clone(),
                role: Some(unit.role),
                source_excerpt: Some(unit.source_text.chars().take(480).collect()),
                rejected_output_excerpt: Some(unit.translated_text.chars().take(480).collect()),
            });
        }
        translated
            .units
            .retain(|unit| !fallback_ids.contains(&unit.id));
        layout = build_layout_with_options(&translated, LayoutOptions::application_default());
    }
    PreparedTranslationLayout {
        translated,
        layout,
        failures,
    }
}

fn extract_single_pdf_page(
    source_path: &Path,
    page_index: usize,
) -> Result<Vec<u8>, PdfTranslationError> {
    let source_path_text = source_path.to_string_lossy();
    let source = MuPdfDocument::open(source_path_text.as_ref()).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Preparing,
            "Could not open the source PDF for page streaming",
            error,
        )
    })?;
    extract_single_pdf_page_from_document(&source, page_index)
}

fn extract_single_pdf_page_from_document(
    source: &MuPdfDocument,
    page_index: usize,
) -> Result<Vec<u8>, PdfTranslationError> {
    let page = source.find_page(page_index as i32).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Preparing,
            "Could not isolate a source PDF page",
            error,
        )
    })?;
    let mut output = MuPdfDocument::new();
    let mut graft = output.new_graft_map().map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Preparing,
            "Could not initialize a PDF page copy",
            error,
        )
    })?;
    let page = graft.graft_object(&page).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Preparing,
            "Could not copy a source PDF page",
            error,
        )
    })?;
    output.insert_page(0, &page).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Preparing,
            "Could not assemble a single-page PDF",
            error,
        )
    })?;
    let mut bytes = Vec::new();
    output.write_to(&mut bytes).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Preparing,
            "Could not serialize a single-page PDF",
            error,
        )
    })?;
    Ok(bytes)
}

fn reindex_preview_layout(page: &LayoutPage) -> LayoutDocument {
    let mut page = page.clone();
    page.page_index = 0;
    for item in &mut page.items {
        item.page_index = 0;
        for object in &mut item.inline_objects {
            object.page_index = 0;
        }
    }
    LayoutDocument {
        pages: vec![page],
        warnings: Vec::new(),
        preserved_on_overflow: Vec::new(),
        overflow_policy: LayoutOptions::application_default().overflow_policy,
    }
}

fn normalize_layout_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn normalize_searchable_text(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn request_fingerprint(
    settings: &Settings,
    request: &PdfTranslationRequest,
) -> Result<String, PdfTranslationError> {
    request_fingerprint_with_source(settings, request).map(|(fingerprint, _, _)| fingerprint)
}

fn request_fingerprint_with_source(
    settings: &Settings,
    request: &PdfTranslationRequest,
) -> Result<(String, String, Vec<u8>), PdfTranslationError> {
    let source_bytes = fs::read(&request.source_path).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Preparing,
            "Could not read the source PDF",
            error,
        )
    })?;
    let source_fingerprint = source_preparation_fingerprint(settings, &source_bytes);
    let mut translation_hasher = DefaultHasher::new();
    PIPELINE_VERSION.hash(&mut translation_hasher);
    source_fingerprint.hash(&mut translation_hasher);
    request.source_language.hash(&mut translation_hasher);
    request.target_language.hash(&mut translation_hasher);
    settings
        .translation
        .active_provider
        .hash(&mut translation_hasher);
    translation_provider_identity(settings).hash(&mut translation_hasher);
    Ok((
        format!("{:016x}", translation_hasher.finish()),
        source_fingerprint,
        source_bytes,
    ))
}

fn source_preparation_fingerprint(settings: &Settings, source_bytes: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    PIPELINE_VERSION.hash(&mut hasher);
    source_bytes.hash(&mut hasher);
    settings.ocr.mode.as_str().hash(&mut hasher);
    settings.ocr.provider.as_str().hash(&mut hasher);
    match settings.ocr.provider {
        OcrProvider::Unlimited => {
            settings.ocr.unlimited.endpoint.hash(&mut hasher);
        }
        OcrProvider::Paddle => {
            settings.ocr.paddle.endpoint.hash(&mut hasher);
            settings.ocr.paddle.model.hash(&mut hasher);
        }
    }
    settings
        .translation
        .content_review
        .mode
        .as_str()
        .hash(&mut hasher);
    if settings.translation.content_review.mode == PdfContentReviewMode::Ambiguous
        && let Some(model) = &settings.translation.content_review.model
    {
        model.provider.as_str().hash(&mut hasher);
        model.id.hash(&mut hasher);
        if let Some(review) = settings.ai.resolved_model(model) {
            review.base_url.hash(&mut hasher);
            review.api_style.as_str().hash(&mut hasher);
        }
    }
    format!("{:016x}", hasher.finish())
}

fn translation_provider_identity(settings: &Settings) -> String {
    let provider = settings.translation.active_provider;
    if provider == glyphst_settings::TranslationProvider::Ai {
        let resolved = settings.ai.resolved_translation();
        return format!(
            "{}|{}|{}",
            resolved.provider.as_str(),
            resolved.model.trim(),
            normalize_endpoint(&resolved.base_url)
        );
    }
    let config = settings.translation.provider_config(provider);
    format!(
        "{}|{}|{}",
        provider.as_str(),
        normalize_endpoint(
            config
                .base_url
                .as_deref()
                .or_else(|| provider.default_base_url())
                .unwrap_or_default()
        ),
        config.region.as_deref().unwrap_or_default().trim()
    )
}

fn normalize_endpoint(endpoint: &str) -> String {
    endpoint.trim().trim_end_matches('/').to_string()
}

fn load_normalized_document(path: &Path, fingerprint: &str) -> Option<DocumentV1> {
    let document = serde_json::from_slice::<DocumentV1>(&fs::read(path).ok()?).ok()?;
    (document.metadata.document_id == fingerprint && document.validate().is_ok())
        .then_some(document)
}

fn load_translation_checkpoint(
    path: &Path,
    identity: &TranslationCheckpointIdentity,
) -> Vec<TranslatedUnit> {
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };
    let Ok(checkpoint) = serde_json::from_slice::<TranslationCheckpoint>(&bytes) else {
        return Vec::new();
    };
    if checkpoint.version == CHECKPOINT_VERSION && checkpoint.identity == *identity {
        checkpoint.resolved_units
    } else {
        Vec::new()
    }
}

fn write_translation_checkpoint(
    path: &Path,
    identity: &TranslationCheckpointIdentity,
    snapshot: &TranslationRunSnapshot,
) -> Result<(), PdfTranslationError> {
    write_json_atomically(
        path,
        &TranslationCheckpoint {
            version: CHECKPOINT_VERSION,
            identity: identity.clone(),
            resolved_units: snapshot.resolved_units.clone(),
            failures: snapshot.failures.clone(),
            progress: snapshot.progress.clone(),
            queue: snapshot.queue.clone(),
        },
        PdfTranslationStage::Translating,
    )
}

fn write_translation_diagnostics(
    path: &Path,
    identity: &TranslationCheckpointIdentity,
    snapshot: &TranslationRunSnapshot,
) -> Result<(), PdfTranslationError> {
    write_json_atomically(
        path,
        &TranslationDiagnostics {
            version: CHECKPOINT_VERSION,
            identity: identity.clone(),
            failures: snapshot.failures.clone(),
            progress: snapshot.progress.clone(),
            queue: snapshot.queue.clone(),
        },
        PdfTranslationStage::Translating,
    )
}

fn translation_checkpoint_changed(
    resolved: usize,
    last_resolved: usize,
    failed: usize,
    last_failed: usize,
) -> bool {
    resolved != last_resolved || failed != last_failed
}

fn create_job_directory(fingerprint: &str) -> Result<PathBuf, PdfTranslationError> {
    let directory = artifact_root().join(fingerprint);
    create_private_directory(&directory).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Preparing,
            "Could not create the PDF translation job directory",
            error,
        )
    })?;
    Ok(directory)
}

fn create_prepared_directory(fingerprint: &str) -> Result<PathBuf, PdfTranslationError> {
    let directory = artifact_root().join("prepared").join(fingerprint);
    create_private_directory(&directory).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Preparing,
            "Could not create the PDF preparation cache directory",
            error,
        )
    })?;
    Ok(directory)
}

fn artifact_root() -> PathBuf {
    glyphst_core::paths::state_dir().join("pdf-translations")
}

fn legacy_artifact_root() -> PathBuf {
    glyphst_core::paths::state_dir().join("document-translation")
}

fn remove_artifact_directory(path: &Path) -> anyhow::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn write_json(path: impl AsRef<Path>, value: &impl Serialize) -> Result<(), PdfTranslationError> {
    let path = path.as_ref();
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Preparing,
            "Could not serialize a PDF translation artifact",
            error,
        )
    })?;
    fs::write(path, bytes).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Preparing,
            "Could not write a PDF translation artifact",
            error,
        )
    })
}

fn write_json_atomically(
    path: &Path,
    value: &impl Serialize,
    stage: PdfTranslationStage,
) -> Result<(), PdfTranslationError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        PdfTranslationError::stage(stage, "Could not serialize a translation checkpoint", error)
    })?;
    let parent = path.parent().ok_or_else(|| {
        PdfTranslationError::message(stage, "Translation checkpoint path has no parent")
    })?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("checkpoint.json");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(".{file_name}.{nonce:x}.tmp"));
    fs::write(&temporary, bytes).map_err(|error| {
        PdfTranslationError::stage(stage, "Could not stage a translation checkpoint", error)
    })?;
    replace_file(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        PdfTranslationError::stage(stage, "Could not publish a translation checkpoint", error)
    })
}

fn publish_atomically(source: &Path, destination: &Path) -> Result<(), PdfTranslationError> {
    let parent = destination.parent().ok_or_else(|| {
        PdfTranslationError::message(
            PdfTranslationStage::Validating,
            "Translated PDF output path has no parent directory",
        )
    })?;
    let file_name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("translated.pdf");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(".{file_name}.glyphst-{nonce:x}.tmp"));
    fs::copy(source, &temporary).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Validating,
            "Could not stage the translated PDF",
            error,
        )
    })?;
    replace_file(&temporary, destination).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        PdfTranslationError::stage(
            PdfTranslationStage::Validating,
            "Could not publish the translated PDF",
            error,
        )
    })
}

fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        fs::rename(source, destination)
    }
    #[cfg(not(unix))]
    {
        match fs::rename(source, destination) {
            Ok(()) => Ok(()),
            Err(_) if destination.exists() => {
                fs::remove_file(destination)?;
                fs::rename(source, destination)
            }
            Err(error) => Err(error),
        }
    }
}

fn create_private_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;

        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hover_test_block(
        id: &str,
        kind: BlockKind,
        bbox: BoundingBox,
    ) -> glyphst_pdf_translation::Block {
        glyphst_pdf_translation::Block {
            id: id.to_string(),
            page_index: 1,
            order: 0,
            reading_order: 0,
            kind,
            role: if kind == BlockKind::Text {
                glyphst_pdf_translation::BlockRole::Body
            } else {
                glyphst_pdf_translation::BlockRole::Image
            },
            bbox,
            content: glyphst_pdf_translation::BlockContent {
                text: (kind == BlockKind::Text)
                    .then_some("Source title")
                    .unwrap_or_default()
                    .to_string(),
                ..glyphst_pdf_translation::BlockContent::default()
            },
            policy: if kind == BlockKind::Text {
                glyphst_pdf_translation::TranslationPolicy::translate("test")
            } else {
                glyphst_pdf_translation::TranslationPolicy::preserve("test")
            },
            continuation: None,
            structure: None,
            toc_entry: None,
            source: glyphst_pdf_translation::SourceProvenance {
                provider: "test".to_string(),
                raw_label: format!("{kind:?}"),
                raw_bbox: bbox,
                raw_path: None,
            },
            tags: Vec::new(),
            metadata: Default::default(),
        }
    }

    #[test]
    fn single_page_pdf_extraction_uses_the_requested_page() {
        let mut source = MuPdfDocument::new();
        source.new_page(mupdf::Size::A4).unwrap();
        source.new_page(mupdf::Size::A5).unwrap();

        let bytes = extract_single_pdf_page_from_document(&source, 1).unwrap();
        let extracted = MuPdfDocument::from_bytes(&bytes).unwrap();

        assert_eq!(extracted.page_count().unwrap(), 1);
        let bounds = extracted.load_pdf_page(0).unwrap().bounds().unwrap();
        assert_eq!(bounds.width(), mupdf::Size::A5.width);
        assert_eq!(bounds.height(), mupdf::Size::A5.height);
    }

    #[test]
    fn page_ocr_worker_pool_never_exceeds_three_requests() {
        assert_eq!(bounded_ocr_worker_count(0), 0);
        assert_eq!(bounded_ocr_worker_count(1), 1);
        assert_eq!(bounded_ocr_worker_count(3), 3);
        assert_eq!(bounded_ocr_worker_count(4), 3);
        assert_eq!(bounded_ocr_worker_count(100), 3);
    }

    #[test]
    fn page_hover_regions_keep_original_text_and_non_text_content_targetable() {
        let text_bounds = BoundingBox::new(20.0, 30.0, 220.0, 58.0);
        let image_bounds = BoundingBox::new(20.0, 80.0, 220.0, 260.0);
        let protected_bounds = BoundingBox::new(20.0, 280.0, 220.0, 440.0);
        let document = DocumentV1::new(
            DocumentMetadata {
                document_id: "hover-regions".to_string(),
                source_path: PathBuf::from("source.pdf"),
                provider: "test".to_string(),
                provider_version: "1".to_string(),
                extra: Default::default(),
            },
            vec![Page {
                page_index: 1,
                geometry: glyphst_pdf_translation::PageGeometry::new(240.0, 480.0),
                blocks: vec![
                    hover_test_block("title", BlockKind::Text, text_bounds),
                    hover_test_block("image", BlockKind::Image, image_bounds),
                    hover_test_block("protected-image-block", BlockKind::Image, protected_bounds),
                ],
                inline_objects: Vec::new(),
                protected_regions: vec![glyphst_pdf_translation::ProtectedRegion {
                    id: "hero".to_string(),
                    page_index: 1,
                    kind: ProtectedRegionKind::Image,
                    bbox: protected_bounds,
                    source: "test".to_string(),
                    confidence: 1.0,
                    reason: "test image".to_string(),
                }],
                metadata: Default::default(),
            }],
        );
        let translated = TranslatedDocument {
            document,
            units: vec![TranslatedUnit {
                id: "title-unit".to_string(),
                member_block_ids: vec!["title".to_string()],
                role: glyphst_pdf_translation::BlockRole::Title,
                source_text: "Source title".to_string(),
                translated_text: "Translated title".to_string(),
                styled_runs: Vec::new(),
                style_fallback: false,
                semantic_hash: 1,
            }],
        };
        // No layout item models the explicit "show original" state. The
        // semantic region must survive so the user can toggle it back.
        let layout_page = LayoutPage {
            page_index: 1,
            geometry: glyphst_pdf_translation::PageGeometry::new(240.0, 480.0),
            items: Vec::new(),
        };

        let regions = build_page_hover_regions(&translated, &layout_page);

        let title = regions
            .iter()
            .find(|region| region.id == "page:1:block:title")
            .unwrap();
        assert_eq!(title.kind, PdfTranslationHoverRegionKind::Text);
        assert_eq!(title.unit_id.as_deref(), Some("title-unit"));
        assert_eq!(title.target_bounds, text_bounds);
        assert!(regions.iter().any(|region| {
            region.id == "page:1:block:image"
                && region.kind == PdfTranslationHoverRegionKind::Image
                && region.target_bounds == image_bounds
        }));
        assert!(regions.iter().any(|region| {
            region.id == "page:1:protected:hero"
                && region.kind == PdfTranslationHoverRegionKind::Image
                && region.target_bounds == protected_bounds
        }));
        assert!(
            !regions
                .iter()
                .any(|region| region.id == "page:1:block:protected-image-block")
        );
    }

    #[test]
    fn final_preview_reconciliation_rebuilds_pages_for_removed_units() {
        let block_pages = HashMap::from([("p003-b0001".to_string(), 2usize)]);
        let unit = TranslatedUnit {
            id: "unit-p003-b0001".to_string(),
            member_block_ids: vec!["p003-b0001".to_string()],
            role: crate::BlockRole::Body,
            source_text: "Source".to_string(),
            translated_text: "Translated".to_string(),
            styled_runs: Vec::new(),
            style_fallback: false,
            semantic_hash: 7,
        };
        let (known, initially_dirty) =
            reconcile_preview_units(&block_pages, &HashMap::new(), std::slice::from_ref(&unit));
        assert_eq!(initially_dirty, BTreeSet::from([2]));

        let (next, removed_dirty) = reconcile_preview_units(&block_pages, &known, &[]);

        assert!(next.is_empty());
        assert_eq!(removed_dirty, BTreeSet::from([2]));
    }

    #[test]
    fn fatal_layout_warnings_stop_pdf_generation() {
        let translated = TranslatedDocument {
            document: DocumentV1::new(
                crate::DocumentMetadata {
                    document_id: "layout-warning".to_string(),
                    source_path: PathBuf::from("source.pdf"),
                    provider: "test".to_string(),
                    provider_version: "1".to_string(),
                    extra: Default::default(),
                },
                Vec::new(),
            ),
            units: Vec::new(),
        };
        let layout = LayoutDocument {
            pages: Vec::new(),
            warnings: vec![glyphst_pdf_translation::LayoutWarning {
                unit_id: "unit".to_string(),
                message: "translated layout box overlaps protected chart region".to_string(),
                fatal: true,
            }],
            preserved_on_overflow: Vec::new(),
            overflow_policy: glyphst_pdf_translation::OverflowPolicy::Adaptive,
        };

        let error = validate_layout_completeness(&translated, &layout).unwrap_err();

        assert!(error.to_string().contains("Unsafe translated layout"));
    }

    #[test]
    fn fingerprint_does_not_include_ocr_or_translation_secrets() {
        let mut settings = Settings::default();
        settings.ocr.paddle.token = "paddle-secret".to_string();
        settings.ocr.unlimited.api_key = "baidu-api-key-secret".to_string();
        settings.ocr.unlimited.secret_key = "baidu-secret-key-secret".to_string();
        settings
            .translation
            .provider_config_mut(glyphst_settings::TranslationProvider::Google)
            .api_key = "google-secret".to_string();
        let source_path = PathBuf::from("/home/meowking/Downloads/test.pdf");
        if !source_path.is_file() {
            return;
        }
        let request = PdfTranslationRequest {
            source_path,
            output_path: "/tmp/translated.pdf".into(),
            source_language: None,
            target_language: LanguageCode::new("zh-hans").unwrap(),
        };

        let fingerprint = request_fingerprint(&settings, &request).unwrap();

        assert!(!fingerprint.contains("paddle-secret"));
        assert!(!fingerprint.contains("baidu-api-key-secret"));
        assert!(!fingerprint.contains("baidu-secret-key-secret"));
        assert!(!fingerprint.contains("google-secret"));
    }

    #[test]
    fn fingerprint_tracks_enabled_content_review_configuration() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let source_path =
            std::env::temp_dir().join(format!("glyphst-review-fingerprint-{nonce:x}.pdf"));
        fs::write(&source_path, b"fingerprint fixture").unwrap();
        let request = PdfTranslationRequest {
            source_path: source_path.clone(),
            output_path: "/tmp/translated.pdf".into(),
            source_language: None,
            target_language: LanguageCode::new("zh-hans").unwrap(),
        };
        let mut settings = Settings::default();
        let off = request_fingerprint(&settings, &request).unwrap();
        settings.translation.content_review.model = Some(glyphst_settings::ModelRef::new(
            glyphst_settings::AiProvider::OpenAi,
            "review-a",
        ));
        assert_eq!(off, request_fingerprint(&settings, &request).unwrap());

        settings.translation.content_review.mode = PdfContentReviewMode::Ambiguous;
        let review_a = request_fingerprint(&settings, &request).unwrap();
        settings.translation.content_review.model = Some(glyphst_settings::ModelRef::new(
            glyphst_settings::AiProvider::OpenAi,
            "review-b",
        ));
        let review_b = request_fingerprint(&settings, &request).unwrap();

        assert_ne!(off, review_a);
        assert_ne!(review_a, review_b);
        fs::remove_file(source_path).unwrap();
    }

    #[test]
    fn preparation_cache_identity_is_independent_of_target_language() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let source_path =
            std::env::temp_dir().join(format!("glyphst-source-fingerprint-{nonce:x}.pdf"));
        fs::write(&source_path, b"same immutable source bytes").unwrap();
        let settings = Settings::default();
        let request = |target: &str| PdfTranslationRequest {
            source_path: source_path.clone(),
            output_path: "/tmp/translated.pdf".into(),
            source_language: Some(LanguageCode::new("en").unwrap()),
            target_language: LanguageCode::new(target).unwrap(),
        };

        let (zh_translation, zh_source, _) =
            request_fingerprint_with_source(&settings, &request("zh-hans")).unwrap();
        let (ja_translation, ja_source, _) =
            request_fingerprint_with_source(&settings, &request("ja")).unwrap();

        assert_eq!(zh_source, ja_source);
        assert_ne!(zh_translation, ja_translation);
        fs::remove_file(source_path).unwrap();
    }

    #[test]
    fn normalized_document_cache_requires_matching_fingerprint() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("glyphst-document-cache-{nonce:x}"));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("document.v1.json");
        let document = DocumentV1::new(
            crate::DocumentMetadata {
                document_id: "matching".to_string(),
                source_path: PathBuf::from("source.pdf"),
                provider: "test".to_string(),
                provider_version: "1".to_string(),
                extra: Default::default(),
            },
            Vec::new(),
        );
        write_json(&path, &document).unwrap();

        assert_eq!(load_normalized_document(&path, "matching"), Some(document));
        assert!(load_normalized_document(&path, "different").is_none());
    }

    #[test]
    fn translation_failure_report_includes_unit_location_and_excerpts() {
        let request = PdfTranslationRequest {
            source_path: "/tmp/source.pdf".into(),
            output_path: "/tmp/translated.pdf".into(),
            source_language: Some(LanguageCode::new("en").unwrap()),
            target_language: LanguageCode::new("zh-hans").unwrap(),
        };
        let identity = TranslationCheckpointIdentity {
            pipeline_version: PIPELINE_VERSION.to_string(),
            fingerprint: "fingerprint".to_string(),
            document_id: "document".to_string(),
            source_language: Some("en".to_string()),
            target_language: "zh-hans".to_string(),
            provider_identity: "google|https://user:secret@example.test/v1?key=secret|us"
                .to_string(),
        };
        let snapshot = TranslationRunSnapshot {
            resolved_units: Vec::new(),
            failures: vec![TranslationFailureDiagnostic {
                id: "unit-p015-b0002".to_string(),
                phase: TranslationFailurePhase::Translation,
                reason: "translation failed validation".to_string(),
                attempts: 3,
                retryable: true,
                category: Some("invalid_response".to_string()),
                status: None,
                member_block_ids: vec!["p015-b0002".to_string()],
                role: Some(crate::BlockRole::Body),
                source_excerpt: Some("Source excerpt around Dunn.".to_string()),
                rejected_output_excerpt: Some("Rejected output excerpt.".to_string()),
            }],
            progress: TranslationRunProgress {
                translated: 0,
                cached: 322,
                retrying: 0,
                failed: 1,
                total: 329,
            },
            queue: TranslationQueueStats {
                planned_requests: 1,
                dispatched_requests: 3,
                completed_requests: 3,
                retry_requests: 2,
                validation_retry_requests: 2,
                ..TranslationQueueStats::default()
            },
        };
        let locations = HashMap::from([(
            "p015-b0002".to_string(),
            TranslationBlockLocation {
                page_index: 14,
                bbox: BoundingBox::new(10.0, 20.0, 300.0, 80.0),
            },
        )]);
        let error = TranslationDocumentError {
            message: "1 translation unit remains unresolved".to_string(),
            unresolved_unit_ids: vec!["unit-p015-b0002".to_string()],
            retryable: true,
        };

        let report = translation_failure_report(
            &request,
            "fingerprint",
            Path::new("/tmp/job"),
            Path::new("/tmp/job/translation-diagnostics.json"),
            &identity,
            &locations,
            Some(&snapshot),
            &error,
        );

        assert!(report.contains("unit-p015-b0002"));
        assert!(report.contains("page 15, block p015-b0002"));
        assert!(report.contains("bbox (10.00, 20.00)-(300.00, 80.00)"));
        assert!(report.contains("Source excerpt around Dunn."));
        assert!(report.contains("Rejected output excerpt."));
        assert!(report.contains("google|https://example.test/v1|us"));
        assert!(!report.contains("secret"));
    }

    #[test]
    fn matching_checkpoint_resumes_and_mismatched_identity_is_ignored() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("glyphst-checkpoint-{nonce:x}"));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("translated-document.partial.json");
        let identity = TranslationCheckpointIdentity {
            pipeline_version: PIPELINE_VERSION.to_string(),
            fingerprint: "fingerprint".to_string(),
            document_id: "document".to_string(),
            source_language: Some("en".to_string()),
            target_language: "fr".to_string(),
            provider_identity: "ai|model|endpoint".to_string(),
        };
        let unit = TranslatedUnit {
            id: "unit-1".to_string(),
            member_block_ids: vec!["block-1".to_string()],
            role: crate::BlockRole::Body,
            source_text: "source".to_string(),
            translated_text: "target".to_string(),
            styled_runs: Vec::new(),
            style_fallback: false,
            semantic_hash: 1,
        };
        let snapshot = TranslationRunSnapshot {
            resolved_units: vec![unit.clone()],
            failures: Vec::new(),
            progress: TranslationRunProgress {
                translated: 1,
                cached: 0,
                retrying: 0,
                failed: 0,
                total: 2,
            },
            queue: TranslationQueueStats {
                planned_requests: 2,
                completed_requests: 1,
                ..TranslationQueueStats::default()
            },
        };

        write_translation_checkpoint(&path, &identity, &snapshot).unwrap();
        let mut updated = snapshot;
        updated.resolved_units[0].translated_text = "updated target".to_string();
        write_translation_checkpoint(&path, &identity, &updated).unwrap();

        let mut expected = unit;
        expected.translated_text = "updated target".to_string();
        assert_eq!(load_translation_checkpoint(&path, &identity), [expected]);
        let mut mismatched = identity.clone();
        mismatched.target_language = "de".to_string();
        assert!(load_translation_checkpoint(&path, &mismatched).is_empty());
        assert_eq!(
            fs::read_dir(&directory).unwrap().count(),
            1,
            "atomic checkpoint write left a staging file behind"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn every_newly_resolved_unit_advances_the_checkpoint() {
        assert!(translation_checkpoint_changed(1, 0, 0, 0));
        assert!(translation_checkpoint_changed(8, 7, 1, 1));
        assert!(translation_checkpoint_changed(7, 7, 1, 0));
        assert!(!translation_checkpoint_changed(7, 7, 1, 1));
    }

    #[test]
    fn clearing_translation_data_removes_current_and_legacy_artifacts() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("glyphst-clear-cache-{nonce:x}"));
        let artifacts = directory.join("pdf-translations");
        let legacy = directory.join("document-translation");
        fs::create_dir_all(artifacts.join("job")).unwrap();
        fs::create_dir_all(legacy.join("ocr")).unwrap();
        fs::write(artifacts.join("job/translated.pdf"), b"pdf").unwrap();
        fs::write(legacy.join("ocr/result.json"), b"{}").unwrap();
        let service = PdfTranslationService::new(TranslationService::new().unwrap());

        service
            .clear_translation_data_at(&artifacts, &legacy, false)
            .unwrap();
        assert!(!artifacts.exists());
        assert!(!legacy.exists());

        service
            .clear_translation_data_at(&artifacts, &legacy, false)
            .unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn active_translation_prevents_artifact_clearing() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("glyphst-active-cache-{nonce:x}"));
        let artifacts = directory.join("pdf-translations");
        let legacy = directory.join("document-translation");
        fs::create_dir_all(&artifacts).unwrap();
        fs::write(artifacts.join("checkpoint.json"), b"{}").unwrap();
        let service = PdfTranslationService::new(TranslationService::new().unwrap());
        let run = service.begin_run().unwrap();

        let error = service
            .clear_translation_data_at(&artifacts, &legacy, false)
            .unwrap_err();
        assert!(error.to_string().contains("PDF translation is active"));
        assert!(artifacts.exists());

        drop(run);
        service
            .clear_translation_data_at(&artifacts, &legacy, false)
            .unwrap();
        assert!(!artifacts.exists());
        fs::remove_dir_all(directory).unwrap();
    }
}
