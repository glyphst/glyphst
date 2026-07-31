use std::collections::{HashMap, hash_map::DefaultHasher};
use std::fmt::{self, Write as _};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, anyhow};
use glyphst_settings::{OcrMode, OcrProvider, PdfContentReviewMode, Settings};
use glyphst_translation::{LanguageCode, TranslationService};
use mupdf::{Document, TextPageFlags};
use serde::{Deserialize, Serialize};

use glyphst_pdf_translation::{
    BoundingBox, CancellationToken, DocumentV1, LayoutDocument, LayoutOptions, PaddleClient,
    PaddleClientConfig, PdfComposer, PdfTranslationStage, ProgressTracker, TranslatedDocument,
    TranslationProgressSink, TypstCompiler, TypstCompilerConfig, UnlimitedClient,
    UnlimitedClientConfig, annotate_reference_sections, build_layout_with_options,
    extract_native_document, fuse_native_with_ocr_report, inspect_pdf, normalize_paddle_payload,
    normalize_unlimited_payload, review_translation_policies, sanitize_source_pdf,
};
use glyphst_pdf_translation::{
    TranslatedUnit, TranslationDocumentError, TranslationFailureDiagnostic, TranslationQueueStats,
    TranslationRunProgress, TranslationRunSnapshot, translate_document_resuming,
};

use crate::backend::GlyphstTranslationBackend;
use crate::reviewer::GlyphstPolicyReviewer;

const ARTIFACT_VERSION: u32 = 5;
const PIPELINE_VERSION: &str = "native-rust-v23-inline-object-anchors-baselines";
const CHECKPOINT_VERSION: u32 = 2;

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
        let (fingerprint, source_bytes) = request_fingerprint_with_source(&settings, &request)?;
        let job_directory = create_job_directory(&fingerprint)?;
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
        let mut progress = ProgressTracker::new(progress_sink);
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

        let document_path = job_directory.join("document.v1.json");
        let mut document = if let Some(document) =
            load_normalized_document(&document_path, &fingerprint)
        {
            progress.finish_stage(PdfTranslationStage::Ocr, "Reused extracted document");
            progress.finish_stage(
                PdfTranslationStage::Normalizing,
                "Reused normalized document",
            );
            document
        } else {
            match settings.ocr.mode {
                OcrMode::Always => self.run_selected_ocr(
                    &settings,
                    &pipeline_request,
                    &target_pages,
                    &job_directory,
                    &cancellation,
                    &mut progress,
                )?,
                OcrMode::Auto if inspection.recommends_ocr => self.run_selected_ocr(
                    &settings,
                    &pipeline_request,
                    &target_pages,
                    &job_directory,
                    &cancellation,
                    &mut progress,
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
                        fingerprint.clone(),
                        &cancellation,
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
            }
        };
        annotate_reference_sections(&mut document);
        let mut warnings = Vec::new();
        if let Some(reviewer) = GlyphstPolicyReviewer::from_settings(&settings) {
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
        }
        write_json(&document_path, &document)?;

        progress.report(
            PdfTranslationStage::Translating,
            "Preparing semantic translation units",
            Some(0),
            Some(1),
        );
        let backend = GlyphstTranslationBackend::new(
            self.translation.clone(),
            settings.clone(),
            request.source_language.clone(),
            request.target_language.clone(),
        );
        let checkpoint_identity =
            TranslationCheckpointIdentity::new(&settings, &request, &fingerprint, &document);
        let checkpoint_path = job_directory.join("translated-document.partial.json");
        let diagnostics_path = job_directory.join("translation-diagnostics.json");
        let block_locations = translation_block_locations(&document);
        let resumed_units = load_translation_checkpoint(&checkpoint_path, &checkpoint_identity);
        let mut last_checkpoint_resolved = resumed_units.len();
        let mut last_terminal_failures = 0usize;
        let mut final_translation_counts = TranslationRunProgress::default();
        let mut final_translation_snapshot = None;
        let translation_result = translate_document_resuming(
            document,
            &backend,
            &cancellation,
            resumed_units,
            |snapshot| {
                let counts = &snapshot.progress;
                final_translation_counts = counts.clone();
                final_translation_snapshot = Some(snapshot.clone());
                let resolved = snapshot.resolved_units.len();
                let should_persist = translation_checkpoint_changed(
                    resolved,
                    last_checkpoint_resolved,
                    counts.failed,
                    last_terminal_failures,
                );
                if should_persist {
                    write_translation_checkpoint(&checkpoint_path, &checkpoint_identity, snapshot)
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
        let translated = match translation_result {
            Ok(translated) => translated,
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
        progress.report_translation(
            "Document translation complete",
            final_translation_counts.translated as u32,
            final_translation_counts.cached as u32,
            0,
            0,
            final_translation_counts.total as u32,
        );
        write_json(job_directory.join("translated-document.json"), &translated)?;

        progress.report(
            PdfTranslationStage::Layout,
            "Fitting translated text to source geometry",
            Some(0),
            Some(1),
        );
        let layout = build_layout_with_options(&translated, LayoutOptions::application_default());
        validate_layout_completeness(&translated, &layout)?;
        write_json(job_directory.join("layout.json"), &layout)?;
        progress.finish_stage(PdfTranslationStage::Layout, "Translated layout prepared");

        progress.report(
            PdfTranslationStage::Compiling,
            "Compiling the translated PDF with Typst",
            None,
            None,
        );
        let sanitized_source_path = job_directory.join("source-sanitized.pdf");
        sanitize_source_pdf(
            &pipeline_request.source_path,
            &sanitized_source_path,
            &layout,
            &cancellation,
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
                &pipeline_request.source_path,
                &sanitized_source_path,
                &layout,
                &job_directory,
                &cancellation,
            )
            .map_err(|error| PdfTranslationError {
                stage: PdfTranslationStage::Compiling,
                message: error.message,
                retryable: false,
                detailed_message: None,
            })?;
        write_json(job_directory.join("layout.json"), &layout)?;
        progress.finish_stage(PdfTranslationStage::Compiling, "Typst PDF compiled");

        progress.report(
            PdfTranslationStage::Composing,
            "Restoring PDF links, outlines, and metadata",
            Some(0),
            Some(1),
        );
        let composed_path = job_directory.join("translated-final.pdf");
        self.composer
            .compose(
                &pipeline_request.source_path,
                &typst_output.pdf_path,
                &composed_path,
                &layout,
                &inspection,
                &cancellation,
            )
            .map_err(|error| PdfTranslationError {
                stage: PdfTranslationStage::Composing,
                message: error.message,
                retryable: false,
                detailed_message: None,
            })?;
        progress.finish_stage(PdfTranslationStage::Composing, "PDF structure restored");

        progress.report(
            PdfTranslationStage::Validating,
            "Validating translated PDF",
            Some(0),
            Some(1),
        );
        validate_output(
            &pipeline_request.source_path,
            &composed_path,
            &translated,
            &layout,
        )?;
        cancellation
            .check()
            .map_err(|_| PdfTranslationError::cancelled(PdfTranslationStage::Validating))?;
        publish_atomically(&composed_path, &request.output_path)?;
        let manifest = build_manifest(
            &settings,
            &request,
            &fingerprint,
            &job_directory,
            &layout,
            &translated,
            &warnings,
        );
        let manifest_path = job_directory.join("artifact-manifest.json");
        write_json(&manifest_path, &manifest)?;
        progress.finish("Translated PDF is ready");

        Ok(PdfTranslationResult {
            output_path: request.output_path,
            manifest_path,
            job_directory,
            fingerprint,
            warnings,
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

    fn run_paddle_ocr(
        &self,
        settings: &Settings,
        request: &PdfTranslationRequest,
        target_pages: &[crate::PageGeometry],
        job_directory: &Path,
        cancellation: &CancellationToken,
        progress: &mut ProgressTracker,
    ) -> Result<DocumentV1, PdfTranslationError> {
        progress.report(
            PdfTranslationStage::Ocr,
            "Submitting the document to PaddleOCR",
            None,
            None,
        );
        let client = PaddleClient::new(
            PaddleClientConfig {
                base_url: settings.ocr.paddle.endpoint.clone(),
                ..PaddleClientConfig::default()
            },
            settings.ocr.paddle.token.clone(),
        )
        .map_err(|error| {
            PdfTranslationError::message(PdfTranslationStage::Ocr, error.to_string())
        })?;
        let result = client
            .run_document(
                &request.source_path,
                &settings.ocr.paddle.model,
                cancellation,
                |ocr_progress| {
                    let completed = ocr_progress
                        .extracted_pages
                        .and_then(|value| u32::try_from(value).ok());
                    let total = ocr_progress
                        .total_pages
                        .and_then(|value| u32::try_from(value).ok());
                    progress.report(
                        PdfTranslationStage::Ocr,
                        match ocr_progress.state.as_str() {
                            "pending" => "PaddleOCR is queued".to_string(),
                            "running" => "PaddleOCR is parsing the document".to_string(),
                            _ => format!("PaddleOCR state: {}", ocr_progress.state),
                        },
                        completed,
                        total,
                    );
                },
            )
            .map_err(|error| PdfTranslationError {
                stage: PdfTranslationStage::Ocr,
                message: error.to_string(),
                retryable: matches!(
                    error.category,
                    crate::PaddleErrorCategory::QueueFull
                        | crate::PaddleErrorCategory::ServiceUnavailable
                        | crate::PaddleErrorCategory::PollTimeout
                ),
                detailed_message: None,
            })?;
        write_json(job_directory.join("paddle-raw.json"), &result.payload)?;
        progress.finish_stage(PdfTranslationStage::Ocr, "PaddleOCR result downloaded");

        progress.report(
            PdfTranslationStage::Normalizing,
            "Normalizing OCR geometry and semantic roles",
            Some(0),
            Some(1),
        );
        let document = normalize_paddle_payload(
            &result.payload,
            request.source_path.clone(),
            request_fingerprint(settings, request).unwrap_or_else(|_| "document".to_string()),
            settings.ocr.paddle.model.clone(),
            target_pages,
        )
        .map_err(|error| {
            PdfTranslationError::stage(
                PdfTranslationStage::Normalizing,
                "Could not normalize the PaddleOCR result",
                error,
            )
        })?;
        let document = self.fuse_ocr_with_native(document, request, job_directory, cancellation)?;
        progress.finish_stage(PdfTranslationStage::Normalizing, "OCR document normalized");
        Ok(document)
    }

    fn run_selected_ocr(
        &self,
        settings: &Settings,
        request: &PdfTranslationRequest,
        target_pages: &[crate::PageGeometry],
        job_directory: &Path,
        cancellation: &CancellationToken,
        progress: &mut ProgressTracker,
    ) -> Result<DocumentV1, PdfTranslationError> {
        match settings.ocr.provider {
            OcrProvider::Unlimited => self.run_unlimited_ocr(
                settings,
                request,
                target_pages,
                job_directory,
                cancellation,
                progress,
            ),
            OcrProvider::Paddle => self.run_paddle_ocr(
                settings,
                request,
                target_pages,
                job_directory,
                cancellation,
                progress,
            ),
        }
    }

    fn run_unlimited_ocr(
        &self,
        settings: &Settings,
        request: &PdfTranslationRequest,
        target_pages: &[crate::PageGeometry],
        job_directory: &Path,
        cancellation: &CancellationToken,
        progress: &mut ProgressTracker,
    ) -> Result<DocumentV1, PdfTranslationError> {
        progress.report(
            PdfTranslationStage::Ocr,
            "Submitting the document to Unlimited-OCR",
            None,
            None,
        );
        let client = UnlimitedClient::new(
            UnlimitedClientConfig {
                base_url: settings.ocr.unlimited.endpoint.clone(),
                ..UnlimitedClientConfig::default()
            },
            settings.ocr.unlimited.api_key.clone(),
            settings.ocr.unlimited.secret_key.clone(),
        )
        .map_err(|error| {
            PdfTranslationError::message(PdfTranslationStage::Ocr, error.to_string())
        })?;
        let result = client
            .run_document(&request.source_path, cancellation, |ocr_progress| {
                progress.report(
                    PdfTranslationStage::Ocr,
                    match ocr_progress.state.as_str() {
                        "pending" => "Unlimited-OCR is queued".to_string(),
                        "running" => "Unlimited-OCR is parsing the document".to_string(),
                        _ => format!("Unlimited-OCR state: {}", ocr_progress.state),
                    },
                    None,
                    None,
                );
            })
            .map_err(|error| PdfTranslationError {
                stage: PdfTranslationStage::Ocr,
                message: error.to_string(),
                retryable: matches!(
                    error.category,
                    crate::UnlimitedErrorCategory::QueueFull
                        | crate::UnlimitedErrorCategory::ServiceUnavailable
                        | crate::UnlimitedErrorCategory::PollTimeout
                ),
                detailed_message: None,
            })?;
        write_json(job_directory.join("unlimited-raw.json"), &result.payload)?;
        progress.finish_stage(PdfTranslationStage::Ocr, "Unlimited-OCR result downloaded");

        progress.report(
            PdfTranslationStage::Normalizing,
            "Normalizing OCR geometry and semantic roles",
            Some(0),
            Some(1),
        );
        let document = normalize_unlimited_payload(
            &result.payload,
            request.source_path.clone(),
            request_fingerprint(settings, request).unwrap_or_else(|_| "document".to_string()),
            target_pages,
        )
        .map_err(|error| {
            PdfTranslationError::stage(
                PdfTranslationStage::Normalizing,
                "Could not normalize the Unlimited-OCR result",
                error,
            )
        })?;
        let document = self.fuse_ocr_with_native(document, request, job_directory, cancellation)?;
        progress.finish_stage(PdfTranslationStage::Normalizing, "OCR document normalized");
        Ok(document)
    }

    fn fuse_ocr_with_native(
        &self,
        document: DocumentV1,
        request: &PdfTranslationRequest,
        job_directory: &Path,
        cancellation: &CancellationToken,
    ) -> Result<DocumentV1, PdfTranslationError> {
        if let Ok(native_document) =
            extract_native_document(&request.source_path, "native-geometry", cancellation)
        {
            let fusion = fuse_native_with_ocr_report(native_document, &document);
            write_json(
                job_directory.join("fusion-diagnostics.json"),
                &fusion.report,
            )?;
            Ok(fusion.document)
        } else {
            Ok(document)
        }
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

fn validate_output(
    source_path: &Path,
    output_path: &Path,
    translated: &TranslatedDocument,
    layout: &LayoutDocument,
) -> Result<(), PdfTranslationError> {
    validate_layout_completeness(translated, layout)?;
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
    request_fingerprint_with_source(settings, request).map(|(fingerprint, _)| fingerprint)
}

fn request_fingerprint_with_source(
    settings: &Settings,
    request: &PdfTranslationRequest,
) -> Result<(String, Vec<u8>), PdfTranslationError> {
    let source_bytes = fs::read(&request.source_path).map_err(|error| {
        PdfTranslationError::stage(
            PdfTranslationStage::Preparing,
            "Could not read the source PDF",
            error,
        )
    })?;
    let mut hasher = DefaultHasher::new();
    PIPELINE_VERSION.hash(&mut hasher);
    request.source_path.hash(&mut hasher);
    source_bytes.hash(&mut hasher);
    request.source_language.hash(&mut hasher);
    request.target_language.hash(&mut hasher);
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
    settings.translation.active_provider.hash(&mut hasher);
    translation_provider_identity(settings).hash(&mut hasher);
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
    Ok((format!("{:016x}", hasher.finish()), source_bytes))
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
