use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, hash_map::DefaultHasher};
use std::fmt::{self, Write as _};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc as sync_mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, anyhow};
use glyphst_pdf::DocumentSnapshot;
#[cfg(test)]
use glyphst_settings::Settings;
use glyphst_settings::{OcrMode, OcrProvider, PdfContentReviewMode};
use glyphst_translation::{LanguageCode, TranslationService};
use mupdf::pdf::PdfDocument as MuPdfDocument;
use mupdf::{
    Document, TextPageFlags,
    text_page::{TextBlockType, TextCharFlags},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use glyphst_pdf_translation::{
    BlockKind, BoundingBox, CancellationToken, DocumentMetadata, DocumentV1, LayoutDocument,
    LayoutOptions, LayoutPage, OcrFusionReport, PaddleClient, PaddleClientConfig, Page,
    PdfComposer, PdfInspection, PdfTranslationStage, ProgressTracker, ProtectedRegionKind,
    TranslatedDocument, TranslationProgressSink, TypstCompiler, TypstCompilerConfig,
    UnlimitedClient, UnlimitedClientConfig, annotate_reference_sections, build_layout_with_options,
    compose_side_by_side, extract_native_document, fuse_native_with_ocr_report, inspect_pdf,
    normalize_paddle_payload, normalize_unlimited_payload, reconcile_provider_continuations,
    review_translation_policies, sanitize_source_pdf, source_text_regions,
};
use glyphst_pdf_translation::{
    ScopedRun, TranslatedUnit, TranslationDocumentError, TranslationFailureDiagnostic,
    TranslationFailurePhase, TranslationQueueStats, TranslationRunProgress, TranslationRunSnapshot,
    TranslationScope, translate_document_scoped,
};

use crate::PdfTranslationConfig;
use crate::backend::GlyphstTranslationBackend;
use crate::reviewer::GlyphstPolicyReviewer;

const ARTIFACT_VERSION: u32 = 7;
const PIPELINE_VERSION: &str = "native-rust-v26-page-streaming-canonical-previews";
const CHECKPOINT_VERSION: u32 = 4;
const LIVE_SESSION_CACHE_VERSION: u32 = 1;
const LIVE_SESSION_MANIFEST_FILE: &str = "live-session.json";
const LIVE_SESSION_STATE_FILE: &str = "live-session-state.json";
const LIVE_SESSION_DOCUMENT_FILE: &str = "live-prepared-document.json";
const LIVE_SESSION_RESULT_FILE: &str = "live-session-result.json";

#[derive(Clone)]
pub struct PdfTranslationService {
    translation: TranslationService,
    typst: TypstCompiler,
    composer: PdfComposer,
    artifact_cache_state: Arc<Mutex<ArtifactCacheState>>,
    /// Where translation jobs and their caches are kept.
    artifact_root: PathBuf,
}

#[derive(Default)]
struct ArtifactCacheState {
    active_runs: usize,
}

struct ArtifactRunGuard {
    state: Arc<Mutex<ArtifactCacheState>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct LiveSessionCacheManifest {
    version: u32,
    pipeline_version: String,
    fingerprint: String,
    source_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_sha256: Option<String>,
    request: PdfTranslationSessionRequest,
    inspection: PdfInspection,
    warnings: Vec<PdfTranslationWarning>,
    completed: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct LiveSessionPipelineState {
    units: Option<TranslationRunSnapshot>,
    page_failures: Vec<PdfTranslationPageFailure>,
}

struct LiveSessionCacheWriter {
    job_directory: PathBuf,
    manifest: LiveSessionCacheManifest,
    prepared_metadata: Option<DocumentMetadata>,
    prepared_pages: BTreeMap<usize, Page>,
    ready_prefix: usize,
    state: LiveSessionPipelineState,
}

impl LiveSessionCacheWriter {
    fn new(
        job_directory: PathBuf,
        manifest: LiveSessionCacheManifest,
    ) -> Result<Self, PdfTranslationError> {
        let writer = Self {
            job_directory,
            manifest,
            prepared_metadata: None,
            prepared_pages: BTreeMap::new(),
            ready_prefix: 0,
            state: LiveSessionPipelineState::default(),
        };
        writer.write_manifest()?;
        Ok(writer)
    }

    fn write_manifest(&self) -> Result<(), PdfTranslationError> {
        write_json_atomically(
            &self.job_directory.join(LIVE_SESSION_MANIFEST_FILE),
            &self.manifest,
            PdfTranslationStage::Preparing,
        )
    }

    fn write_state(&self) -> Result<(), PdfTranslationError> {
        write_json_atomically(
            &self.job_directory.join(LIVE_SESSION_STATE_FILE),
            &self.state,
            PdfTranslationStage::Translating,
        )
    }

    fn write_prepared_prefix(&self) -> Result<(), PdfTranslationError> {
        let Some(metadata) = self.prepared_metadata.clone() else {
            return Ok(());
        };
        let pages = (0..self.ready_prefix)
            .map(|page_index| self.prepared_pages.get(&page_index).cloned())
            .collect::<Option<Vec<_>>>();
        let Some(pages) = pages else {
            return Ok(());
        };
        let document = DocumentV1::new(metadata, pages);
        write_json_atomically(
            &self.job_directory.join(LIVE_SESSION_DOCUMENT_FILE),
            &document,
            PdfTranslationStage::Normalizing,
        )
    }

    fn record(&mut self, update: &PdfTranslationSessionUpdate) -> Result<(), PdfTranslationError> {
        match update {
            PdfTranslationSessionUpdate::CacheReady(_) | PdfTranslationSessionUpdate::Finished => {
                Ok(())
            }
            PdfTranslationSessionUpdate::PagePrepared {
                metadata,
                page,
                page_count: _,
                ready_prefix,
            } => {
                self.prepared_metadata = Some(metadata.clone());
                self.prepared_pages.insert(page.page_index, page.clone());
                self.ready_prefix = *ready_prefix;
                self.write_prepared_prefix()
            }
            PdfTranslationSessionUpdate::UnitsChanged(units) => {
                self.state.units = Some(units.clone());
                self.write_state()
            }
            PdfTranslationSessionUpdate::PagePreviewReady(preview) => {
                let previous_len = self.state.page_failures.len();
                self.state.page_failures.retain(|failure| {
                    failure.page_index != preview.page_index
                        || !matches!(
                            failure.phase,
                            TranslationFailurePhase::Layout | TranslationFailurePhase::Preview
                        )
                });
                if self.state.page_failures.len() == previous_len {
                    Ok(())
                } else {
                    self.write_state()
                }
            }
            PdfTranslationSessionUpdate::PageFailed(failure) => {
                self.state.page_failures.retain(|current| {
                    current.page_index != failure.page_index || current.phase != failure.phase
                });
                self.state.page_failures.push(failure.clone());
                self.state
                    .page_failures
                    .sort_by_key(|failure| (failure.page_index, failure.phase as u8));
                self.write_state()
            }
            PdfTranslationSessionUpdate::Reconciled {
                ready_prefix,
                invalidated_unit_ids,
                ..
            } => {
                self.ready_prefix = *ready_prefix;
                if let Some(units) = self.state.units.as_mut()
                    && !invalidated_unit_ids.is_empty()
                {
                    let invalidated = invalidated_unit_ids.iter().collect::<HashSet<_>>();
                    units
                        .resolved_units
                        .retain(|unit| !invalidated.contains(&unit.id));
                    units
                        .failures
                        .retain(|failure| !invalidated.contains(&failure.id));
                    self.write_state()?;
                }
                self.write_prepared_prefix()
            }
        }
    }

    fn finish(&mut self, result: &PdfTranslationSessionResult) -> Result<(), PdfTranslationError> {
        self.state.units = Some(TranslationRunSnapshot {
            resolved_units: result.translated.units.clone(),
            failures: result.failures.clone(),
            progress: result.progress.clone(),
            queue: result.queue.clone(),
        });
        self.write_state()?;
        write_json_atomically(
            &self.job_directory.join(LIVE_SESSION_RESULT_FILE),
            result,
            PdfTranslationStage::Validating,
        )?;
        self.manifest.warnings = result.warnings.clone();
        self.manifest.completed = true;
        self.write_manifest()
    }
}

impl Drop for ArtifactRunGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.active_runs = state.active_runs.saturating_sub(1);
        }
    }
}

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
            artifact_root: artifact_root(),
        }
    }

    /// Keeps translation artifacts under `root` instead of the state
    /// directory (tests, scripted runs).
    pub fn with_artifact_root(mut self, root: PathBuf) -> Self {
        self.artifact_root = root;
        self
    }

    pub fn clear_translation_data(&self) -> anyhow::Result<()> {
        self.clear_translation_data_at(&self.artifact_root, true)
    }

    fn clear_translation_data_at(
        &self,
        artifact_root: &Path,
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
        if let Err(error) = remove_artifact_directory(artifact_root) {
            errors.push(format!(
                "could not clear PDF translation artifacts: {error:#}"
            ));
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
        settings: PdfTranslationConfig,
        request: PdfTranslationRequest,
        cancellation: CancellationToken,
        progress_sink: impl TranslationProgressSink + 'static,
    ) -> Result<PdfTranslationResult, PdfTranslationError> {
        self.run_with_snapshot(settings, request, None, cancellation, progress_sink)
    }

    pub fn run_from_snapshot(
        &self,
        settings: PdfTranslationConfig,
        request: PdfTranslationRequest,
        source_snapshot: Arc<DocumentSnapshot>,
        cancellation: CancellationToken,
        progress_sink: impl TranslationProgressSink + 'static,
    ) -> Result<PdfTranslationResult, PdfTranslationError> {
        self.run_with_snapshot(
            settings,
            request,
            Some(source_snapshot.as_ref()),
            cancellation,
            progress_sink,
        )
    }

    fn run_with_snapshot(
        &self,
        settings: PdfTranslationConfig,
        request: PdfTranslationRequest,
        source_snapshot: Option<&DocumentSnapshot>,
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
            source_snapshot,
            &TranslationScope::whole(),
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
                layout: ExportLayout::Translated,
            },
            &cancellation,
            &mut progress,
        )
    }

    pub fn run_session(
        &self,
        settings: PdfTranslationConfig,
        request: PdfTranslationSessionRequest,
        cancellation: CancellationToken,
        progress_sink: impl TranslationProgressSink + 'static,
        update_sink: impl FnMut(PdfTranslationSessionUpdate) + Send,
    ) -> Result<PdfTranslationSessionResult, PdfTranslationError> {
        self.run_session_with_snapshot(
            settings,
            request,
            None,
            &TranslationScope::whole(),
            cancellation,
            progress_sink,
            update_sink,
        )
    }

    pub fn run_session_from_snapshot(
        &self,
        settings: PdfTranslationConfig,
        request: PdfTranslationSessionRequest,
        source_snapshot: Arc<DocumentSnapshot>,
        cancellation: CancellationToken,
        progress_sink: impl TranslationProgressSink + 'static,
        update_sink: impl FnMut(PdfTranslationSessionUpdate) + Send,
    ) -> Result<PdfTranslationSessionResult, PdfTranslationError> {
        self.run_scoped_session_from_snapshot(
            settings,
            request,
            source_snapshot,
            &TranslationScope::whole(),
            cancellation,
            progress_sink,
            update_sink,
        )
    }

    /// A session that translates only what `scope` covers, following it as
    /// it moves; it finishes once the scope is whole and translated, or is
    /// cancelled.
    #[allow(clippy::too_many_arguments)]
    pub fn run_scoped_session_from_snapshot(
        &self,
        settings: PdfTranslationConfig,
        request: PdfTranslationSessionRequest,
        source_snapshot: Arc<DocumentSnapshot>,
        scope: &TranslationScope,
        cancellation: CancellationToken,
        progress_sink: impl TranslationProgressSink + 'static,
        update_sink: impl FnMut(PdfTranslationSessionUpdate) + Send,
    ) -> Result<PdfTranslationSessionResult, PdfTranslationError> {
        self.run_session_with_snapshot(
            settings,
            request,
            Some(source_snapshot.as_ref()),
            scope,
            cancellation,
            progress_sink,
            update_sink,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn run_session_with_snapshot(
        &self,
        settings: PdfTranslationConfig,
        request: PdfTranslationSessionRequest,
        source_snapshot: Option<&DocumentSnapshot>,
        scope: &TranslationScope,
        cancellation: CancellationToken,
        progress_sink: impl TranslationProgressSink + 'static,
        mut update_sink: impl FnMut(PdfTranslationSessionUpdate) + Send,
    ) -> Result<PdfTranslationSessionResult, PdfTranslationError> {
        let _run_guard = self.begin_run()?;
        let mut progress = ProgressTracker::new(progress_sink);
        self.run_session_inner(
            &settings,
            request,
            source_snapshot,
            scope,
            &cancellation,
            &mut progress,
            &mut update_sink,
        )
    }

    /// Restore a live session strictly from Glyphst's private translation
    /// artifacts. This path never calls OCR, translation, or review providers.
    pub fn restore_cached_session(
        &self,
        key: &PdfTranslationSessionCacheKey,
        request: &PdfTranslationSessionRequest,
        original_unit_ids: &[String],
        cancellation: CancellationToken,
        progress_sink: impl TranslationProgressSink + 'static,
        update_sink: impl FnMut(PdfTranslationSessionUpdate) + Send,
    ) -> Result<Option<PdfTranslationRestoredSession>, PdfTranslationError> {
        self.restore_cached_session_with_snapshot(
            key,
            request,
            None,
            original_unit_ids,
            cancellation,
            progress_sink,
            update_sink,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn restore_cached_session_from_snapshot(
        &self,
        key: &PdfTranslationSessionCacheKey,
        request: &PdfTranslationSessionRequest,
        source_snapshot: Arc<DocumentSnapshot>,
        original_unit_ids: &[String],
        cancellation: CancellationToken,
        progress_sink: impl TranslationProgressSink + 'static,
        update_sink: impl FnMut(PdfTranslationSessionUpdate) + Send,
    ) -> Result<Option<PdfTranslationRestoredSession>, PdfTranslationError> {
        self.restore_cached_session_with_snapshot(
            key,
            request,
            Some(source_snapshot.as_ref()),
            original_unit_ids,
            cancellation,
            progress_sink,
            update_sink,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn restore_cached_session_with_snapshot(
        &self,
        key: &PdfTranslationSessionCacheKey,
        request: &PdfTranslationSessionRequest,
        source_snapshot: Option<&DocumentSnapshot>,
        original_unit_ids: &[String],
        cancellation: CancellationToken,
        progress_sink: impl TranslationProgressSink + 'static,
        mut update_sink: impl FnMut(PdfTranslationSessionUpdate) + Send,
    ) -> Result<Option<PdfTranslationRestoredSession>, PdfTranslationError> {
        let _run_guard = self.begin_run()?;
        let job_directory = self.artifact_root.join(key.as_str());
        let manifest_path = job_directory.join(LIVE_SESSION_MANIFEST_FILE);
        let manifest = match fs::read(&manifest_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<LiveSessionCacheManifest>(&bytes).ok())
        {
            Some(manifest)
                if manifest.version == LIVE_SESSION_CACHE_VERSION
                    && manifest.pipeline_version == PIPELINE_VERSION
                    && manifest.fingerprint == key.as_str()
                    && manifest.request == *request =>
            {
                manifest
            }
            _ => return Ok(None),
        };
        let source_snapshot_path = job_directory.join("source-original.pdf");
        let current_source_from_path;
        let current_source = match source_snapshot {
            Some(snapshot) if snapshot.path() == request.source_path => snapshot.bytes(),
            Some(_) => return Ok(None),
            None => {
                current_source_from_path = match fs::read(&request.source_path) {
                    Ok(bytes) => bytes,
                    Err(_) => return Ok(None),
                };
                current_source_from_path.as_slice()
            }
        };
        let cached_source = match fs::read(&source_snapshot_path) {
            Ok(bytes) => bytes,
            Err(_) => return Ok(None),
        };
        let source_matches = cached_source_matches(
            manifest.source_sha256.as_deref(),
            current_source,
            &cached_source,
        );
        if !source_matches || !is_unencrypted_pdf(&source_snapshot_path) {
            return Ok(None);
        }
        cancellation
            .check()
            .map_err(|_| PdfTranslationError::cancelled(PdfTranslationStage::Preparing))?;

        let state = fs::read(job_directory.join(LIVE_SESSION_STATE_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<LiveSessionPipelineState>(&bytes).ok())
            .unwrap_or_default();
        let result = if manifest.completed {
            match fs::read(job_directory.join(LIVE_SESSION_RESULT_FILE))
                .ok()
                .and_then(|bytes| {
                    serde_json::from_slice::<PdfTranslationSessionResult>(&bytes).ok()
                })
                .filter(|result| {
                    result.fingerprint == key.as_str()
                        && result.request == *request
                        && result.translated.document.validate().is_ok()
                }) {
                Some(mut result) => {
                    result.source_snapshot_path = source_snapshot_path.clone();
                    result.job_directory = job_directory.clone();
                    result
                }
                None => return Ok(None),
            }
        } else {
            let document = match fs::read(job_directory.join(LIVE_SESSION_DOCUMENT_FILE))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<DocumentV1>(&bytes).ok())
                .filter(|document| document.validate().is_ok())
            {
                Some(document) => document,
                None => return Ok(None),
            };
            let units = state.units.clone().unwrap_or(TranslationRunSnapshot {
                resolved_units: Vec::new(),
                failures: Vec::new(),
                progress: TranslationRunProgress::default(),
                queue: TranslationQueueStats::default(),
            });
            PdfTranslationSessionResult {
                request: request.clone(),
                translated: TranslatedDocument {
                    document,
                    units: units.resolved_units,
                },
                failures: units.failures,
                progress: units.progress,
                queue: units.queue,
                inspection: manifest.inspection.clone(),
                source_snapshot_path: source_snapshot_path.clone(),
                job_directory: job_directory.clone(),
                fingerprint: manifest.fingerprint.clone(),
                warnings: manifest.warnings.clone(),
            }
        };

        let page_count = manifest
            .inspection
            .pages
            .len()
            .max(result.translated.document.pages.len());
        for page in &result.translated.document.pages {
            update_sink(PdfTranslationSessionUpdate::PagePrepared {
                metadata: result.translated.document.metadata.clone(),
                page: page.clone(),
                page_count,
                ready_prefix: page.page_index + 1,
            });
        }
        update_sink(PdfTranslationSessionUpdate::UnitsChanged(
            TranslationRunSnapshot {
                resolved_units: result.translated.units.clone(),
                failures: result.failures.clone(),
                progress: result.progress.clone(),
                queue: result.queue.clone(),
            },
        ));
        let mut restored_page_failures = state.page_failures.clone();
        for failure in &restored_page_failures {
            update_sink(PdfTranslationSessionUpdate::PageFailed(failure.clone()));
        }

        let mut progress = ProgressTracker::new(progress_sink);
        progress.report(
            PdfTranslationStage::Compiling,
            "Restoring cached translation pages",
            Some(0),
            Some(page_count as u32),
        );
        for (completed, page) in result.translated.document.pages.iter().enumerate() {
            cancellation
                .check()
                .map_err(|_| PdfTranslationError::cancelled(PdfTranslationStage::Compiling))?;
            let update = match self.render_session_page_preview(
                &result,
                page.page_index,
                1,
                original_unit_ids,
                &cancellation,
            ) {
                Ok(preview) => {
                    restored_page_failures.retain(|failure| {
                        failure.page_index != page.page_index
                            || !matches!(
                                failure.phase,
                                TranslationFailurePhase::Layout | TranslationFailurePhase::Preview
                            )
                    });
                    PdfTranslationSessionUpdate::PagePreviewReady(preview)
                }
                Err(error) => {
                    let failure = PdfTranslationPageFailure {
                        page_index: page.page_index,
                        phase: if error.stage == PdfTranslationStage::Layout {
                            TranslationFailurePhase::Layout
                        } else {
                            TranslationFailurePhase::Preview
                        },
                        message: error.message,
                        retryable: error.retryable,
                    };
                    restored_page_failures.retain(|current| {
                        current.page_index != failure.page_index || current.phase != failure.phase
                    });
                    restored_page_failures.push(failure.clone());
                    PdfTranslationSessionUpdate::PageFailed(failure)
                }
            };
            update_sink(update);
            progress.report(
                PdfTranslationStage::Compiling,
                format!("Restored page {} of {page_count}", completed + 1),
                Some((completed + 1) as u32),
                Some(page_count as u32),
            );
        }
        update_sink(PdfTranslationSessionUpdate::Finished);
        restored_page_failures.sort_by_key(|failure| (failure.page_index, failure.phase as u8));
        Ok(Some(PdfTranslationRestoredSession {
            result,
            page_failures: restored_page_failures,
            completed: manifest.completed,
        }))
    }

    pub fn export_session(
        &self,
        settings: PdfTranslationConfig,
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

    #[allow(clippy::too_many_arguments)]
    fn run_session_inner(
        &self,
        settings: &PdfTranslationConfig,
        session_request: PdfTranslationSessionRequest,
        source_snapshot: Option<&DocumentSnapshot>,
        scope: &TranslationScope,
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
        let path_snapshot;
        let source_snapshot = match source_snapshot {
            Some(snapshot) if snapshot.path() == request.source_path => snapshot,
            Some(_) => {
                return Err(PdfTranslationError::message(
                    PdfTranslationStage::Preparing,
                    "The authenticated PDF snapshot does not match the translation source",
                ));
            }
            None => {
                let source_bytes = fs::read(&request.source_path).map_err(|error| {
                    PdfTranslationError::stage(
                        PdfTranslationStage::Preparing,
                        "Could not read the source PDF",
                        error,
                    )
                })?;
                path_snapshot = DocumentSnapshot::from_pdf_bytes(
                    request.source_path.clone(),
                    Arc::<[u8]>::from(source_bytes),
                );
                &path_snapshot
            }
        };
        let (fingerprint, source_fingerprint) =
            request_fingerprints_for_source(settings, &request, source_snapshot.bytes());
        let working_source_bytes = source_snapshot.unencrypted_pdf_bytes().map_err(|error| {
            PdfTranslationError::stage(
                PdfTranslationStage::Preparing,
                "Could not prepare an unencrypted source PDF",
                error,
            )
        })?;
        let job_directory = create_job_directory(&self.artifact_root, &fingerprint)?;
        let prepared_directory =
            create_prepared_directory(&self.artifact_root, &source_fingerprint)?;
        let source_path = job_directory.join("source-original.pdf");
        write_private_file(&source_path, &working_source_bytes).map_err(|error| {
            PdfTranslationError::stage(
                PdfTranslationStage::Preparing,
                "Could not snapshot the source PDF",
                error,
            )
        })?;
        if !is_unencrypted_pdf(&source_path) {
            return Err(PdfTranslationError::message(
                PdfTranslationStage::Preparing,
                "The PDF translation working copy is still encrypted",
            ));
        }
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
            inspect_pdf(&pipeline_request.source_path, cancellation).map_err(|error| {
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
        let cache_key = PdfTranslationSessionCacheKey::parse(fingerprint.clone())
            .expect("request fingerprints are lowercase hexadecimal");
        update_sink(PdfTranslationSessionUpdate::CacheReady(cache_key));
        let cache_manifest = LiveSessionCacheManifest {
            version: LIVE_SESSION_CACHE_VERSION,
            pipeline_version: PIPELINE_VERSION.to_string(),
            fingerprint: fingerprint.clone(),
            source_fingerprint: source_fingerprint.clone(),
            source_sha256: Some(source_sha256(source_snapshot.bytes())),
            request: session_request.clone(),
            inspection: inspection.clone(),
            warnings: Vec::new(),
            completed: false,
        };
        let mut cache_writer =
            match LiveSessionCacheWriter::new(job_directory.clone(), cache_manifest) {
                Ok(writer) => Some(writer),
                Err(error) => {
                    eprintln!("failed to initialize live PDF translation cache: {error}");
                    None
                }
            };
        let mut publish_update = |update: PdfTranslationSessionUpdate| {
            if let Some(writer) = cache_writer.as_mut()
                && let Err(error) = writer.record(&update)
            {
                eprintln!("failed to update live PDF translation cache: {error}");
            }
            update_sink(update);
        };
        progress.finish_stage(PdfTranslationStage::Preparing, "Source PDF inspected");
        let backend = GlyphstTranslationBackend::new(
            self.translation.clone(),
            settings.translation.clone(),
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
                    scope,
                    cancellation,
                    progress,
                    &mut publish_update,
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
                    scope,
                    cancellation,
                    progress,
                    &mut publish_update,
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
            && let Some(reviewer) = GlyphstPolicyReviewer::from_config(settings)
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
            publish_update(PdfTranslationSessionUpdate::PageFailed(failure.clone()));
        }
        let page_count = document.pages.len();
        for page in &document.pages {
            publish_update(PdfTranslationSessionUpdate::PagePrepared {
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
            TranslationCheckpointIdentity::new(settings, &request, &fingerprint, &document);
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
        let update_sink = Mutex::new(&mut publish_update);
        let translation_result = std::thread::scope(|threads| {
            let (preview_tx, preview_rx) = sync_mpsc::channel();
            threads.spawn(|| {
                stream_page_previews(
                    self,
                    &document,
                    &pipeline_request.source_path,
                    &inspection,
                    &job_directory,
                    scope,
                    cancellation,
                    streamed_preview_units.clone(),
                    streamed_preview_revisions.clone(),
                    preview_rx,
                    &update_sink,
                );
            });
            let result = translate_document_scoped(
                document.clone(),
                &backend,
                cancellation,
                resumed_units,
                ScopedRun {
                    park_when_idle: true,
                    ..ScopedRun::whole(scope)
                },
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
        let result = PdfTranslationSessionResult {
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
        };
        if let Ok(mut sink) = update_sink.lock() {
            (**sink)(PdfTranslationSessionUpdate::Finished);
        }
        if let Some(writer) = cache_writer.as_mut()
            && let Err(error) = writer.finish(&result)
        {
            eprintln!("failed to finish live PDF translation cache: {error}");
        }
        Ok(result)
    }

    fn export_session_inner(
        &self,
        settings: &PdfTranslationConfig,
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
        let composed_path = match request.layout {
            ExportLayout::Translated => composed_path,
            ExportLayout::SideBySide => {
                let side_by_side = job_directory.join("side-by-side.pdf");
                compose_side_by_side(
                    source_snapshot_path,
                    &composed_path,
                    &side_by_side,
                    cancellation,
                )
                .map_err(|error| {
                    PdfTranslationError::message(
                        PdfTranslationStage::Composing,
                        format!("Could not put the pages side by side: {}", error.message),
                    )
                })?;
                side_by_side
            }
        };
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

    #[allow(clippy::too_many_arguments)]
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
        let mut hover_regions = build_page_hover_regions(translated, &page);
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
        let text_layer = extract_preview_text_layer(&output_path)?;
        refine_page_hover_regions(&mut hover_regions, &page, &text_layer);
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
            text_layer,
            pdf: Arc::from(pdf),
            layout_failures,
        })
    }

    pub fn cached_artifact(
        &self,
        settings: &PdfTranslationConfig,
        request: &PdfTranslationRequest,
    ) -> Option<PdfTranslationResult> {
        let _state = self.artifact_cache_state.lock().ok()?;
        let fingerprint = request_fingerprint(settings, request).ok()?;
        let job_directory = self.artifact_root.join(&fingerprint);
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
        settings: &PdfTranslationConfig,
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
            output_path: self.artifact_root.join(fingerprint).join("translated.pdf"),
            ..provisional
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn run_selected_ocr(
        &self,
        settings: &PdfTranslationConfig,
        request: &PdfTranslationRequest,
        target_pages: &[crate::PageGeometry],
        preparation_directory: &Path,
        preview_job_directory: &Path,
        document_id: &str,
        inspection: &PdfInspection,
        selected_pages: &[usize],
        scope: &TranslationScope,
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
        let client = match PageOcrClient::from_config(settings) {
            Ok(client) => client,
            // Without local OCR set up, the pages keep their own text and
            // say what they need; nothing is sent elsewhere.
            Err(error) if settings.ocr.provider == OcrProvider::Local => {
                for &page_index in &selected_pages {
                    update_sink(PdfTranslationSessionUpdate::PageFailed(
                        PdfTranslationPageFailure {
                            page_index,
                            phase: TranslationFailurePhase::Ocr,
                            message: error.message.clone(),
                            retryable: false,
                        },
                    ));
                }
                progress.finish_stage(PdfTranslationStage::Ocr, "Native PDF text extracted");
                progress.finish_stage(
                    PdfTranslationStage::Normalizing,
                    "Native document normalized",
                );
                return Ok(native);
            }
            Err(error) => return Err(error),
        };
        let tasks = selected_pages
            .iter()
            .map(|page_index| PageOcrTask {
                page_index: *page_index,
                pdf: extract_single_pdf_page(&request.source_path, *page_index),
            })
            .collect::<Vec<_>>();
        // Pages left to recognise, taken most urgent first: a session
        // following the reader recognises the pages around it, and waits
        // for the reader (or the whole document) for the rest.
        let remaining = Mutex::new(selected_pages.iter().copied().collect::<BTreeSet<_>>());
        let task_of = tasks
            .iter()
            .map(|task| (task.page_index, task))
            .collect::<HashMap<_, _>>();
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
        let contiguous =
            |ready: &[Option<Page>]| ready.iter().take_while(|page| page.is_some()).count();
        for page in ready_pages.iter().flatten() {
            update_sink(PdfTranslationSessionUpdate::PagePrepared {
                metadata: stream_metadata.clone(),
                page: page.clone(),
                page_count: ready_pages.len(),
                ready_prefix: contiguous(&ready_pages),
            });
        }
        std::thread::scope(|threads| {
            for _ in 0..worker_count {
                let sender = sender.clone();
                let client = client.clone();
                let native = &native;
                let (remaining, task_of) = (&remaining, &task_of);
                threads.spawn(move || {
                    while !cancellation.is_cancelled() {
                        let view = scope.view();
                        let next = {
                            let mut remaining = remaining
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner());
                            if remaining.is_empty() {
                                break;
                            }
                            take_ocr_page(&mut remaining, &view)
                        };
                        let Some(page) = next else {
                            scope.wait_changed(view.generation(), OCR_SCOPE_POLL);
                            continue;
                        };
                        let result = run_ocr_page(
                            &client,
                            task_of[&page],
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
            drop(sender);
            // Translate what is ready as pages come in, and when the reader
            // moves to pages that needed no recognition.
            let mut translated_at = None::<u64>;
            let mut completed = 0usize;
            loop {
                let received = receiver.recv_timeout(OCR_SCOPE_POLL);
                let result = match received {
                    Ok(result) => Some(result),
                    Err(sync_mpsc::RecvTimeoutError::Timeout) => None,
                    Err(sync_mpsc::RecvTimeoutError::Disconnected) => break,
                };
                if cancellation.is_cancelled() {
                    continue;
                }
                if let Some(result) = result {
                    completed += 1;
                    progress.report(
                        PdfTranslationStage::Ocr,
                        format!(
                            "OCR page {} finished ({completed}/{})",
                            result.page_index + 1,
                            tasks.len()
                        ),
                        Some(completed as u32),
                        Some(tasks.len() as u32),
                    );
                    let page_index = result.page_index;
                    let page = result
                        .page
                        .clone()
                        .unwrap_or_else(|| native.pages[page_index].clone());
                    ready_pages[page_index] = Some(page.clone());
                    if let Some(failure) = &result.failure {
                        update_sink(PdfTranslationSessionUpdate::PageFailed(failure.clone()));
                    }
                    update_sink(PdfTranslationSessionUpdate::PagePrepared {
                        metadata: stream_metadata.clone(),
                        page,
                        page_count: ready_pages.len(),
                        ready_prefix: contiguous(&ready_pages),
                    });
                    results.push(result);
                } else if translated_at == Some(scope.generation()) {
                    continue;
                }
                translated_at = Some(scope.generation());
                translate_ready_pages(
                    self,
                    &ready_pages,
                    &native,
                    &stream_metadata,
                    scope,
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

/// The remaining page to recognise next, most urgent first; `None` while
/// none is in scope.
fn take_ocr_page(
    remaining: &mut BTreeSet<usize>,
    view: &glyphst_pdf_translation::ScopeView,
) -> Option<usize> {
    let page = remaining
        .iter()
        .filter_map(|&page| view.ocr_rank(page).map(|rank| (rank, page)))
        .min()
        .map(|(_, page)| page)?;
    remaining.remove(&page);
    Some(page)
}
/// How often recognition looks at a scope that has nothing for it.
const OCR_SCOPE_POLL: Duration = Duration::from_millis(100);

fn bounded_ocr_worker_count(task_count: usize) -> usize {
    task_count.min(MAX_ACTIVE_OCR_PAGES)
}

#[derive(Clone)]
enum PageOcrClient {
    Paddle { client: PaddleClient, model: String },
    Unlimited(UnlimitedClient),
    Local(Arc<glyphst_ocr::LocalOcr>),
}

impl PageOcrClient {
    fn from_config(settings: &PdfTranslationConfig) -> Result<Self, PdfTranslationError> {
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
            OcrProvider::Local => match glyphst_ocr::LocalOcr::installed() {
                Ok(Some(ocr)) => Ok(Self::Local(ocr)),
                Ok(None) => Err(PdfTranslationError::message(
                    PdfTranslationStage::Ocr,
                    LOCAL_OCR_MISSING,
                )),
                Err(error) => Err(PdfTranslationError::message(
                    PdfTranslationStage::Ocr,
                    format!("Local OCR could not start: {error:#}"),
                )),
            },
        }
    }

    fn provider_name(&self) -> &'static str {
        match self {
            Self::Paddle { .. } => "paddle",
            Self::Unlimited(_) => "unlimited",
            Self::Local(_) => "local",
        }
    }
}

/// Why a page was not recognised while local OCR is not set up.
const LOCAL_OCR_MISSING: &str =
    "This page needs text recognition: install local OCR in Settings · OCR.";

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
        PageOcrClient::Local(ocr) => {
            crate::local_ocr::recognize_page(ocr, bytes).map_err(|error| (error, false))
        }
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
        PageOcrClient::Local(_) => normalize_paddle_payload(
            &payload,
            source_path,
            document_id,
            crate::local_ocr::MODEL,
            &[geometry],
        ),
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

/// One pass over the pages ready so far: the document with every page,
/// those still being recognised standing in as their native text, so
/// paragraphs and references read right; only units wholly on ready pages
/// that the scope covers are translated.
#[allow(clippy::too_many_arguments)]
fn translate_ready_pages(
    service: &PdfTranslationService,
    ready_pages: &[Option<Page>],
    native: &DocumentV1,
    metadata: &DocumentMetadata,
    scope: &TranslationScope,
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
    if cancellation.is_cancelled() || ready_pages.is_empty() {
        return;
    }
    let ready: Vec<bool> = ready_pages.iter().map(Option::is_some).collect();
    let mut document = DocumentV1::new(
        metadata.clone(),
        ready_pages
            .iter()
            .zip(&native.pages)
            .map(|(ready, native)| ready.clone().unwrap_or_else(|| native.clone()))
            .collect(),
    );
    reconcile_provider_continuations(&mut document);
    annotate_reference_sections(&mut document);
    let resumed = streamed_units.clone();
    let is_ready = |page: usize| ready.get(page).copied().unwrap_or(false);
    match translate_document_scoped(
        document,
        backend,
        cancellation,
        resumed,
        ScopedRun {
            scope,
            ready: &is_ready,
            park_when_idle: false,
            stop: None,
        },
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
            for page_index in preview_order(dirty_pages, &scope.view()) {
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

#[allow(clippy::too_many_arguments)]
fn stream_page_previews<U>(
    service: &PdfTranslationService,
    document: &DocumentV1,
    source_path: &Path,
    inspection: &PdfInspection,
    job_directory: &Path,
    scope: &TranslationScope,
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
        // A page's first preview goes out at once; later revisions wait for
        // the burst of results to settle.
        let first_preview = dirty_pages.iter().any(|page| !revisions.contains_key(page));
        let wait = match (first_dirty_at, last_change_at) {
            _ if first_preview => Duration::ZERO,
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
                    scope,
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
                    scope,
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
    scope: &TranslationScope,
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
    let view = scope.view();
    let pages = preview_order(std::mem::take(dirty_pages), &view);
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

/// Dirty pages, the reader's first.
fn preview_order(pages: BTreeSet<usize>, view: &glyphst_pdf_translation::ScopeView) -> Vec<usize> {
    let mut pages = pages.into_iter().collect::<Vec<_>>();
    pages.sort_by_key(|&page| (view.rank(page).unwrap_or(u32::MAX), page));
    pages
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfTranslationRequest {
    pub source_path: PathBuf,
    pub output_path: PathBuf,
    pub source_language: Option<LanguageCode>,
    pub target_language: LanguageCode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfTranslationSessionRequest {
    pub source_path: PathBuf,
    pub source_language: Option<LanguageCode>,
    pub target_language: LanguageCode,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PdfTranslationSessionUpdate {
    CacheReady(PdfTranslationSessionCacheKey),
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
                translated: true,
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
            translated: layout_item.is_some(),
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
            translated: true,
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
                translated: false,
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

const TRANSLATION_HOVER_TEXT_PADDING_PT: f32 = 1.5;
const TRANSLATION_HOVER_TEXT_TOLERANCE_PT: f32 = 0.75;

fn extract_preview_text_layer(
    output_path: &Path,
) -> Result<PdfTranslationPageTextLayer, PdfTranslationError> {
    let document = Document::open(output_path).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Compiling,
            "Could not reopen the translated page preview",
            error,
        )
    })?;
    let page_count = document.page_count().map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Compiling,
            "Could not count translated page preview pages",
            error,
        )
    })?;
    if page_count != 1 {
        return Err(PdfTranslationError::message(
            PdfTranslationStage::Compiling,
            "Translated page preview did not contain exactly one page",
        ));
    }
    let page = document.load_page(0).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Compiling,
            "Could not load the translated page preview",
            error,
        )
    })?;
    let text_page = page
        .to_text_page(TextPageFlags::PRESERVE_WHITESPACE)
        .map_err(|error| {
            PdfTranslationError::stage(
                PdfTranslationStage::Compiling,
                "Could not extract translated page preview text geometry",
                error,
            )
        })?;
    let mut blocks = Vec::new();
    for block in text_page.blocks() {
        if block.r#type() != TextBlockType::Text {
            continue;
        }
        let mut lines = Vec::new();
        for line in block.lines() {
            let mut glyphs = Vec::new();
            for character in line.chars() {
                let Some(value) = character.char() else {
                    continue;
                };
                let quad = character.quad();
                glyphs.push(PdfTranslationPageGlyph {
                    character: value,
                    quad: PdfTranslationPageQuad {
                        upper_left: PdfTranslationPagePoint::new(quad.ul.x, quad.ul.y),
                        upper_right: PdfTranslationPagePoint::new(quad.ur.x, quad.ur.y),
                        lower_left: PdfTranslationPagePoint::new(quad.ll.x, quad.ll.y),
                        lower_right: PdfTranslationPagePoint::new(quad.lr.x, quad.lr.y),
                    },
                    font_size: character.size(),
                    bold: character.flags().contains(TextCharFlags::BOLD)
                        || character.font().is_some_and(|font| font.is_bold()),
                });
            }
            if glyphs.is_empty() {
                continue;
            }
            let bounds = line.bounds();
            lines.push(PdfTranslationPageTextLine {
                bounds: BoundingBox::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1),
                glyphs,
            });
        }
        if !lines.is_empty() {
            blocks.push(PdfTranslationPageTextBlock { lines });
        }
    }
    Ok(PdfTranslationPageTextLayer { blocks })
}

fn refine_page_hover_regions(
    regions: &mut [PdfTranslationHoverRegion],
    layout_page: &LayoutPage,
    text_layer: &PdfTranslationPageTextLayer,
) {
    let items = layout_page
        .items
        .iter()
        .map(|item| (item.block_id.as_str(), item.bbox))
        .collect::<HashMap<_, _>>();
    for region in regions {
        if region.kind != PdfTranslationHoverRegionKind::Text {
            continue;
        }
        let Some(item_bounds) = region
            .block_id
            .as_deref()
            .and_then(|block_id| items.get(block_id))
            .copied()
        else {
            continue;
        };
        if let Some(glyph_bounds) = text_layer.glyph_envelope_inside(item_bounds) {
            region.target_bounds = BoundingBox::new(
                glyph_bounds.x0 - TRANSLATION_HOVER_TEXT_PADDING_PT,
                glyph_bounds.y0 - TRANSLATION_HOVER_TEXT_PADDING_PT,
                glyph_bounds.x1 + TRANSLATION_HOVER_TEXT_PADDING_PT,
                glyph_bounds.y1 + TRANSLATION_HOVER_TEXT_PADDING_PT,
            )
            .clamp_to(item_bounds);
        }
    }
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
    pub text_layer: PdfTranslationPageTextLayer,
    pub pdf: Arc<[u8]>,
    pub layout_failures: Vec<TranslationFailureDiagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PdfTranslationPagePoint {
    pub x: f32,
    pub y: f32,
}

impl PdfTranslationPagePoint {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PdfTranslationPageQuad {
    pub upper_left: PdfTranslationPagePoint,
    pub upper_right: PdfTranslationPagePoint,
    pub lower_left: PdfTranslationPagePoint,
    pub lower_right: PdfTranslationPagePoint,
}

impl PdfTranslationPageQuad {
    fn bounds(&self) -> BoundingBox {
        let points = [
            self.upper_left,
            self.upper_right,
            self.lower_left,
            self.lower_right,
        ];
        BoundingBox::new(
            points
                .iter()
                .map(|point| point.x)
                .fold(f32::INFINITY, f32::min),
            points
                .iter()
                .map(|point| point.y)
                .fold(f32::INFINITY, f32::min),
            points
                .iter()
                .map(|point| point.x)
                .fold(f32::NEG_INFINITY, f32::max),
            points
                .iter()
                .map(|point| point.y)
                .fold(f32::NEG_INFINITY, f32::max),
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PdfTranslationPageGlyph {
    pub character: char,
    pub quad: PdfTranslationPageQuad,
    pub font_size: f32,
    pub bold: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PdfTranslationPageTextLine {
    pub bounds: BoundingBox,
    pub glyphs: Vec<PdfTranslationPageGlyph>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PdfTranslationPageTextBlock {
    pub lines: Vec<PdfTranslationPageTextLine>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PdfTranslationPageTextLayer {
    pub blocks: Vec<PdfTranslationPageTextBlock>,
}

impl PdfTranslationPageTextLayer {
    fn glyph_envelope_inside(&self, bounds: BoundingBox) -> Option<BoundingBox> {
        self.blocks
            .iter()
            .flat_map(|block| &block.lines)
            .flat_map(|line| &line.glyphs)
            .map(|glyph| glyph.quad.bounds())
            .filter(|glyph| {
                let center_x = (glyph.x0 + glyph.x1) * 0.5;
                let center_y = (glyph.y0 + glyph.y1) * 0.5;
                center_x >= bounds.x0 - TRANSLATION_HOVER_TEXT_TOLERANCE_PT
                    && center_x <= bounds.x1 + TRANSLATION_HOVER_TEXT_TOLERANCE_PT
                    && center_y >= bounds.y0 - TRANSLATION_HOVER_TEXT_TOLERANCE_PT
                    && center_y <= bounds.y1 + TRANSLATION_HOVER_TEXT_TOLERANCE_PT
            })
            .reduce(|left, right| {
                BoundingBox::new(
                    left.x0.min(right.x0),
                    left.y0.min(right.y0),
                    left.x1.max(right.x1),
                    left.y1.max(right.y1),
                )
            })
    }
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
    /// The region's block is shown translated on this page.
    pub translated: bool,
    pub unit_id: Option<String>,
    pub block_id: Option<String>,
    pub target_bounds: BoundingBox,
    pub source_regions: Vec<BoundingBox>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfTranslationPageFailure {
    pub page_index: usize,
    pub phase: TranslationFailurePhase,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PdfTranslationSessionCacheKey(String);

impl PdfTranslationSessionCacheKey {
    pub fn parse(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (value.len() == 16 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PdfTranslationRestoredSession {
    pub result: PdfTranslationSessionResult,
    pub page_failures: Vec<PdfTranslationPageFailure>,
    pub completed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfTranslationExportRequest {
    pub output_path: PathBuf,
    pub original_unit_ids: Vec<String>,
    pub layout: ExportLayout,
}

/// What an exported PDF holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExportLayout {
    /// The translated pages only.
    #[default]
    Translated,
    /// Each original page with its translation to its right.
    SideBySide,
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
        settings: &PdfTranslationConfig,
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

#[allow(clippy::too_many_arguments)]
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

#[allow(clippy::too_many_arguments)]
fn build_manifest(
    settings: &PdfTranslationConfig,
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
        translation_provider: settings.translation.provider().as_str().to_string(),
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
    let output_path_text = output_path.to_string_lossy();
    let output = MuPdfDocument::open(output_path_text.as_ref()).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Validating,
            "Could not open the translated PDF",
            error,
        )
    })?;
    if output.needs_password().map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Validating,
            "Could not inspect translated PDF encryption",
            error,
        )
    })? {
        return Err(PdfTranslationError::message(
            PdfTranslationStage::Validating,
            "Translated PDF must not require a password",
        ));
    }
    let trailer = output.trailer().map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Validating,
            "Could not inspect the translated PDF trailer",
            error,
        )
    })?;
    if trailer
        .get_dict("Encrypt")
        .map_err(|error| {
            PdfTranslationError::stage(
                PdfTranslationStage::Validating,
                "Could not inspect the translated PDF encryption dictionary",
                error,
            )
        })?
        .is_some()
    {
        return Err(PdfTranslationError::message(
            PdfTranslationStage::Validating,
            "Translated PDF must not contain an encryption dictionary",
        ));
    }
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

pub(crate) fn extract_single_pdf_page(
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
    settings: &PdfTranslationConfig,
    request: &PdfTranslationRequest,
) -> Result<String, PdfTranslationError> {
    request_fingerprint_with_source(settings, request).map(|(fingerprint, _, _)| fingerprint)
}

fn request_fingerprint_with_source(
    settings: &PdfTranslationConfig,
    request: &PdfTranslationRequest,
) -> Result<(String, String, Vec<u8>), PdfTranslationError> {
    let source_bytes = fs::read(&request.source_path).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Preparing,
            "Could not read the source PDF",
            error,
        )
    })?;
    let (fingerprint, source_fingerprint) =
        request_fingerprints_for_source(settings, request, &source_bytes);
    Ok((fingerprint, source_fingerprint, source_bytes))
}

fn request_fingerprints_for_source(
    settings: &PdfTranslationConfig,
    request: &PdfTranslationRequest,
    source_bytes: &[u8],
) -> (String, String) {
    let source_fingerprint = source_preparation_fingerprint(settings, source_bytes);
    let mut translation_hasher = DefaultHasher::new();
    PIPELINE_VERSION.hash(&mut translation_hasher);
    source_fingerprint.hash(&mut translation_hasher);
    request.source_language.hash(&mut translation_hasher);
    request.target_language.hash(&mut translation_hasher);
    settings
        .translation
        .provider()
        .hash(&mut translation_hasher);
    translation_provider_identity(settings).hash(&mut translation_hasher);
    (
        format!("{:016x}", translation_hasher.finish()),
        source_fingerprint,
    )
}

fn source_sha256(source_bytes: &[u8]) -> String {
    glyphst_core::lower_hex(Sha256::digest(source_bytes))
}

fn cached_source_matches(
    expected_sha256: Option<&str>,
    current_source: &[u8],
    cached_source: &[u8],
) -> bool {
    expected_sha256.map_or_else(
        || current_source == cached_source,
        |expected| source_sha256(current_source) == expected,
    )
}

fn source_preparation_fingerprint(settings: &PdfTranslationConfig, source_bytes: &[u8]) -> String {
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
        OcrProvider::Local => crate::local_ocr::MODEL.hash(&mut hasher),
    }
    settings.content_review.mode.as_str().hash(&mut hasher);
    if settings.content_review.mode == PdfContentReviewMode::Ambiguous
        && let Some(model) = &settings.content_review.model
    {
        model.provider.as_str().hash(&mut hasher);
        model.id.hash(&mut hasher);
        if let Some(review) = &settings.content_review.resolved {
            review.base_url.hash(&mut hasher);
            review.api_style.as_str().hash(&mut hasher);
        }
    }
    format!("{:016x}", hasher.finish())
}

fn translation_provider_identity(settings: &PdfTranslationConfig) -> String {
    settings
        .translation
        .provider_identity(settings.translation.provider())
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

fn create_job_directory(root: &Path, fingerprint: &str) -> Result<PathBuf, PdfTranslationError> {
    let directory = root.join(fingerprint);
    create_private_directory(&directory).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Preparing,
            "Could not create the PDF translation job directory",
            error,
        )
    })?;
    Ok(directory)
}

fn create_prepared_directory(
    root: &Path,
    fingerprint: &str,
) -> Result<PathBuf, PdfTranslationError> {
    let directory = root.join("prepared").join(fingerprint);
    create_private_directory(&directory).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Preparing,
            "Could not create the PDF preparation cache directory",
            error,
        )
    })?;
    Ok(directory)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        std::io::Write::write_all(&mut file, bytes)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        fs::write(path, bytes)
    }
}

fn is_unencrypted_pdf(path: &Path) -> bool {
    let Some(path) = path.to_str() else {
        return false;
    };
    let Ok(document) = MuPdfDocument::open(path) else {
        return false;
    };
    match document.needs_password() {
        Ok(false) => {}
        Ok(true) | Err(_) => return false,
    }
    document
        .trailer()
        .and_then(|trailer| trailer.get_dict("Encrypt"))
        .is_ok_and(|encrypt| encrypt.is_none())
}

fn artifact_root() -> PathBuf {
    glyphst_core::paths::state_dir().join("pdf-translations-v2")
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
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    use super::*;

    fn encrypted_pdf_bytes() -> Vec<u8> {
        let encoded = include_str!("../../pdf/tests/fixtures/dummy-encrypted.pdf.b64")
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        STANDARD.decode(encoded).unwrap()
    }

    fn temporary_test_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "glyphst-pdf-translation-{label}-{}-{nonce:x}",
            std::process::id()
        ))
    }

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
                text: if kind == BlockKind::Text {
                    "Source title"
                } else {
                    ""
                }
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

    fn hover_test_layout_item(
        block_id: &str,
        bbox: BoundingBox,
    ) -> glyphst_pdf_translation::LayoutItem {
        glyphst_pdf_translation::LayoutItem {
            unit_id: format!("{block_id}-unit"),
            block_id: block_id.to_string(),
            page_index: 0,
            source_bbox: bbox,
            source_regions: vec![bbox],
            structure_group_id: None,
            toc: None,
            bbox,
            text: "Translated".to_string(),
            role: glyphst_pdf_translation::BlockRole::Body,
            source_font_size_pt: 12.0,
            font_size_pt: 12.0,
            minimum_font_size_pt: 8.0,
            emergency_minimum_font_size_pt: 6.0,
            leading_em: 1.2,
            minimum_leading_em: 0.9,
            font_weight: glyphst_pdf_translation::FontWeight::Regular,
            font_class: glyphst_pdf_translation::FontFamilyClass::Serif,
            italic: false,
            underline: false,
            strikeout: false,
            color_argb: 0xff00_0000,
            styled_runs: Vec::new(),
            inline_objects: Vec::new(),
            first_line_indent_pt: 0.0,
            justify: false,
            preserve_line_breaks: false,
        }
    }

    fn preview_test_glyph(character: char, bounds: BoundingBox) -> PdfTranslationPageGlyph {
        PdfTranslationPageGlyph {
            character,
            quad: PdfTranslationPageQuad {
                upper_left: PdfTranslationPagePoint::new(bounds.x0, bounds.y0),
                upper_right: PdfTranslationPagePoint::new(bounds.x1, bounds.y0),
                lower_left: PdfTranslationPagePoint::new(bounds.x0, bounds.y1),
                lower_right: PdfTranslationPagePoint::new(bounds.x1, bounds.y1),
            },
            font_size: 12.0,
            bold: false,
        }
    }

    #[test]
    fn translated_text_hover_uses_the_rendered_glyph_envelope() {
        let allocation = BoundingBox::new(10.0, 10.0, 100.0, 100.0);
        let layer = PdfTranslationPageTextLayer {
            blocks: vec![PdfTranslationPageTextBlock {
                lines: vec![PdfTranslationPageTextLine {
                    bounds: BoundingBox::new(20.0, 30.0, 60.0, 42.0),
                    glyphs: vec![
                        preview_test_glyph('A', BoundingBox::new(20.0, 30.0, 36.0, 42.0)),
                        preview_test_glyph('B', BoundingBox::new(40.0, 30.0, 60.0, 42.0)),
                    ],
                }],
            }],
        };
        let page = LayoutPage {
            page_index: 0,
            geometry: glyphst_pdf_translation::PageGeometry::new(200.0, 200.0),
            items: vec![hover_test_layout_item("title", allocation)],
        };
        let mut regions = vec![PdfTranslationHoverRegion {
            id: "title".to_string(),
            translated: true,
            kind: PdfTranslationHoverRegionKind::Text,
            unit_id: Some("title-unit".to_string()),
            block_id: Some("title".to_string()),
            target_bounds: allocation,
            source_regions: vec![allocation],
        }];

        refine_page_hover_regions(&mut regions, &page, &layer);

        assert_eq!(
            regions[0].target_bounds,
            BoundingBox::new(18.5, 28.5, 61.5, 43.5)
        );
    }

    #[test]
    fn translated_text_hover_keeps_layout_and_non_text_fallbacks() {
        let allocation = BoundingBox::new(10.0, 10.0, 100.0, 100.0);
        let page = LayoutPage {
            page_index: 0,
            geometry: glyphst_pdf_translation::PageGeometry::new(200.0, 200.0),
            items: vec![hover_test_layout_item("title", allocation)],
        };
        let image_bounds = BoundingBox::new(120.0, 20.0, 180.0, 90.0);
        let mut regions = vec![
            PdfTranslationHoverRegion {
                id: "title".to_string(),
                translated: true,
                kind: PdfTranslationHoverRegionKind::Text,
                unit_id: Some("title-unit".to_string()),
                block_id: Some("title".to_string()),
                target_bounds: allocation,
                source_regions: vec![allocation],
            },
            PdfTranslationHoverRegion {
                id: "image".to_string(),
                translated: false,
                kind: PdfTranslationHoverRegionKind::Image,
                unit_id: None,
                block_id: None,
                target_bounds: image_bounds,
                source_regions: vec![image_bounds],
            },
        ];

        refine_page_hover_regions(&mut regions, &page, &PdfTranslationPageTextLayer::default());

        assert_eq!(regions[0].target_bounds, allocation);
        assert_eq!(regions[1].target_bounds, image_bounds);
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
    fn recognition_follows_the_reader_then_takes_the_rest() {
        let scope = TranslationScope::following(5, 1);
        let mut remaining: BTreeSet<usize> = (0..10).collect();
        let mut order = Vec::new();
        while let Some(page) = take_ocr_page(&mut remaining, &scope.view()) {
            order.push(page);
        }
        // The reader's page, a page each side, the window's last page.
        assert_eq!(order, vec![5, 4, 6, 7]);
        scope.expand_to_whole();
        while let Some(page) = take_ocr_page(&mut remaining, &scope.view()) {
            order.push(page);
        }
        assert_eq!(order[4..], [8, 9, 3, 2, 1, 0]);
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

        let fingerprint =
            request_fingerprint(&PdfTranslationConfig::from(&settings), &request).unwrap();

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
        let off = request_fingerprint(&PdfTranslationConfig::from(&settings), &request).unwrap();
        settings.translation.content_review.model = Some(glyphst_settings::ModelRef::new(
            glyphst_settings::AiProvider::OpenAi,
            "review-a",
        ));
        assert_eq!(
            off,
            request_fingerprint(&PdfTranslationConfig::from(&settings), &request).unwrap()
        );

        settings.translation.content_review.mode = PdfContentReviewMode::Ambiguous;
        let review_a =
            request_fingerprint(&PdfTranslationConfig::from(&settings), &request).unwrap();
        settings.translation.content_review.model = Some(glyphst_settings::ModelRef::new(
            glyphst_settings::AiProvider::OpenAi,
            "review-b",
        ));
        let review_b =
            request_fingerprint(&PdfTranslationConfig::from(&settings), &request).unwrap();

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
        let config = PdfTranslationConfig::from(&settings);
        let request = |target: &str| PdfTranslationRequest {
            source_path: source_path.clone(),
            output_path: "/tmp/translated.pdf".into(),
            source_language: Some(LanguageCode::new("en").unwrap()),
            target_language: LanguageCode::new(target).unwrap(),
        };

        let (zh_translation, zh_source, _) =
            request_fingerprint_with_source(&config, &request("zh-hans")).unwrap();
        let (ja_translation, ja_source, _) =
            request_fingerprint_with_source(&config, &request("ja")).unwrap();

        assert_eq!(zh_source, ja_source);
        assert_ne!(zh_translation, ja_translation);
        fs::remove_file(source_path).unwrap();
    }

    #[test]
    fn encrypted_source_identity_matches_its_decrypted_cache_copy() {
        let encrypted = encrypted_pdf_bytes();
        let snapshot = DocumentSnapshot::from_pdf_bytes("source.pdf", encrypted.clone())
            .with_password(glyphst_pdf::DocumentPassword::new("123456"));
        let decrypted = snapshot.unencrypted_pdf_bytes().unwrap();
        let digest = source_sha256(&encrypted);

        assert_ne!(encrypted.as_slice(), decrypted.as_ref());
        assert!(cached_source_matches(Some(&digest), &encrypted, &decrypted));
        assert!(!cached_source_matches(None, &encrypted, &decrypted));
    }

    #[test]
    fn private_working_copy_is_unencrypted_and_owner_only() {
        let directory = temporary_test_directory("private-source");
        create_private_directory(&directory).unwrap();
        let path = directory.join("source-original.pdf");
        let snapshot = DocumentSnapshot::from_pdf_bytes("source.pdf", encrypted_pdf_bytes())
            .with_password(glyphst_pdf::DocumentPassword::new("123456"));
        let decrypted = snapshot.unencrypted_pdf_bytes().unwrap();

        write_private_file(&path, &decrypted).unwrap();

        assert!(is_unencrypted_pdf(&path));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rendered_output_validation_rejects_encryption() {
        let directory = temporary_test_directory("encrypted-output");
        create_private_directory(&directory).unwrap();
        let source_path = directory.join("source.pdf");
        let output_path = directory.join("translated.pdf");
        let snapshot = DocumentSnapshot::from_pdf_bytes("source.pdf", encrypted_pdf_bytes())
            .with_password(glyphst_pdf::DocumentPassword::new("123456"));
        fs::write(&source_path, snapshot.unencrypted_pdf_bytes().unwrap()).unwrap();
        fs::write(&output_path, encrypted_pdf_bytes()).unwrap();
        let layout = LayoutDocument {
            pages: Vec::new(),
            warnings: Vec::new(),
            preserved_on_overflow: Vec::new(),
            overflow_policy: LayoutOptions::application_default().overflow_policy,
        };

        let error = validate_rendered_output(&source_path, &output_path, &layout).unwrap_err();

        assert!(error.message.contains("must not require a password"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn live_session_cache_keys_are_fixed_width_hexadecimal() {
        let key = PdfTranslationSessionCacheKey::parse("0123456789abcdef").unwrap();
        assert_eq!(key.as_str(), "0123456789abcdef");
        assert!(PdfTranslationSessionCacheKey::parse("0123456789abcde").is_none());
        assert!(PdfTranslationSessionCacheKey::parse("../23456789abcdef").is_none());
        assert!(PdfTranslationSessionCacheKey::parse("0123456789abcdeg").is_none());
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
                in_scope: 329,
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
                in_scope: 2,
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
    fn clearing_translation_data_leaves_legacy_artifacts_untouched() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("glyphst-clear-cache-{nonce:x}"));
        let artifacts = directory.join("pdf-translations-v2");
        let legacy = directory.join("document-translation");
        fs::create_dir_all(artifacts.join("job")).unwrap();
        fs::create_dir_all(legacy.join("ocr")).unwrap();
        fs::write(artifacts.join("job/translated.pdf"), b"pdf").unwrap();
        fs::write(legacy.join("ocr/result.json"), b"{}").unwrap();
        let service = PdfTranslationService::new(TranslationService::new().unwrap());

        service
            .clear_translation_data_at(&artifacts, false)
            .unwrap();
        assert!(!artifacts.exists());
        assert_eq!(fs::read(legacy.join("ocr/result.json")).unwrap(), b"{}");

        service
            .clear_translation_data_at(&artifacts, false)
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
        let artifacts = directory.join("pdf-translations-v2");
        fs::create_dir_all(&artifacts).unwrap();
        fs::write(artifacts.join("checkpoint.json"), b"{}").unwrap();
        let service = PdfTranslationService::new(TranslationService::new().unwrap());
        let run = service.begin_run().unwrap();

        let error = service
            .clear_translation_data_at(&artifacts, false)
            .unwrap_err();
        assert!(error.to_string().contains("PDF translation is active"));
        assert!(artifacts.exists());

        drop(run);
        service
            .clear_translation_data_at(&artifacts, false)
            .unwrap();
        assert!(!artifacts.exists());
        fs::remove_dir_all(directory).unwrap();
    }
}
