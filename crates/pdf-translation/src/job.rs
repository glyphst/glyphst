use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use glyphst_settings::{OcrMode, Settings};
use glyphst_translation::{LanguageCode, TranslationService};
use mupdf::{Document, TextPageFlags};
use serde::{Deserialize, Serialize};

use crate::{
    BoundingBox, CancellationToken, DocumentV1, GlyphstTranslationBackend, LayoutDocument,
    PaddleClient, PaddleClientConfig, PdfComposer, PdfTranslationStage, ProgressTracker,
    TranslatedDocument, TranslationProgressSink, TypstCompiler, TypstCompilerConfig, build_layout,
    extract_native_document, inspect_pdf, normalize_paddle_payload, reconcile_ocr_geometry,
    sanitize_source_pdf, translate_document,
};

const ARTIFACT_VERSION: u32 = 1;
const PIPELINE_VERSION: &str = "native-rust-v3";

#[derive(Clone)]
pub struct PdfTranslationService {
    translation: TranslationService,
    typst: TypstCompiler,
    composer: PdfComposer,
}

impl gpui::Global for PdfTranslationService {}

impl PdfTranslationService {
    pub fn new(translation: TranslationService) -> Self {
        Self {
            translation,
            typst: TypstCompiler::new(TypstCompilerConfig::default()),
            composer: PdfComposer,
        }
    }

    pub fn run(
        &self,
        settings: Settings,
        request: PdfTranslationRequest,
        cancellation: CancellationToken,
        progress_sink: impl TranslationProgressSink + 'static,
    ) -> Result<PdfTranslationResult, PdfTranslationError> {
        let fingerprint = request_fingerprint(&settings, &request)?;
        let job_directory = create_job_directory(&fingerprint)?;
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
        let inspection = inspect_pdf(&request.source_path, &cancellation).map_err(|error| {
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

        let document = match settings.ocr.mode {
            OcrMode::Always => self.run_paddle_ocr(
                &settings,
                &request,
                &target_pages,
                &job_directory,
                &cancellation,
                &mut progress,
            )?,
            OcrMode::Auto if inspection.recommends_ocr => self.run_paddle_ocr(
                &settings,
                &request,
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
                    &request.source_path,
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
        };
        write_json(job_directory.join("document.v1.json"), &document)?;

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
        let translated =
            translate_document(document, &backend, &cancellation, |completed, total| {
                progress.report(
                    PdfTranslationStage::Translating,
                    format!("Translated {completed} of {total} semantic units"),
                    Some(completed as u32),
                    Some(total as u32),
                );
            })
            .map_err(|error| PdfTranslationError {
                stage: PdfTranslationStage::Translating,
                message: error.message,
                retryable: !error.unresolved_unit_ids.is_empty(),
            })?;
        progress.finish_stage(
            PdfTranslationStage::Translating,
            "Document translation complete",
        );
        write_json(job_directory.join("translated-document.json"), &translated)?;

        progress.report(
            PdfTranslationStage::Layout,
            "Fitting translated text to source geometry",
            Some(0),
            Some(1),
        );
        let layout = build_layout(&translated);
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
            &request.source_path,
            &sanitized_source_path,
            &layout,
            &cancellation,
        )
        .map_err(|error| PdfTranslationError {
            stage: PdfTranslationStage::Compiling,
            message: format!("Could not remove source text from translated regions: {error}"),
            retryable: false,
        })?;
        let typst_output = self
            .typst
            .compile_translated_pdf(
                &sanitized_source_path,
                &layout,
                &job_directory,
                &cancellation,
            )
            .map_err(|error| PdfTranslationError {
                stage: PdfTranslationStage::Compiling,
                message: error.message,
                retryable: false,
            })?;
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
                &request.source_path,
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
            })?;
        progress.finish_stage(PdfTranslationStage::Composing, "PDF structure restored");

        progress.report(
            PdfTranslationStage::Validating,
            "Validating translated PDF",
            Some(0),
            Some(1),
        );
        validate_output(&request.source_path, &composed_path, &translated, &layout)?;
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
        );
        let manifest_path = job_directory.join("artifact-manifest.json");
        write_json(&manifest_path, &manifest)?;
        progress.finish("Translated PDF is ready");

        Ok(PdfTranslationResult {
            output_path: request.output_path,
            manifest_path,
            job_directory,
            fingerprint,
        })
    }

    pub fn cached_artifact(
        &self,
        settings: &Settings,
        request: &PdfTranslationRequest,
    ) -> Option<PdfTranslationResult> {
        let fingerprint = request_fingerprint(settings, request).ok()?;
        let job_directory = artifact_root().join(&fingerprint);
        let manifest_path = job_directory.join("artifact-manifest.json");
        let manifest: PdfTranslationArtifactManifest =
            serde_json::from_slice(&fs::read(&manifest_path).ok()?).ok()?;
        let output_path = PathBuf::from(manifest.output_path);
        output_path.is_file().then_some(PdfTranslationResult {
            output_path,
            manifest_path,
            job_directory,
            fingerprint,
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
                base_url: settings.ocr.endpoint.clone(),
                ..PaddleClientConfig::default()
            },
            settings.ocr.token.clone(),
        )
        .map_err(|error| {
            PdfTranslationError::message(PdfTranslationStage::Ocr, error.to_string())
        })?;
        let result = client
            .run_document(
                &request.source_path,
                &settings.ocr.model,
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
            })?;
        write_json(job_directory.join("paddle-raw.json"), &result.payload)?;
        progress.finish_stage(PdfTranslationStage::Ocr, "PaddleOCR result downloaded");

        progress.report(
            PdfTranslationStage::Normalizing,
            "Normalizing OCR geometry and semantic roles",
            Some(0),
            Some(1),
        );
        let mut document = normalize_paddle_payload(
            &result.payload,
            request.source_path.clone(),
            request_fingerprint(settings, request).unwrap_or_else(|_| "document".to_string()),
            settings.ocr.model.clone(),
            target_pages,
        )
        .map_err(|error| {
            PdfTranslationError::stage(
                PdfTranslationStage::Normalizing,
                "Could not normalize the PaddleOCR result",
                error,
            )
        })?;
        if let Ok(native_document) =
            extract_native_document(&request.source_path, "native-geometry", cancellation)
        {
            reconcile_ocr_geometry(&mut document, &native_document);
        }
        progress.finish_stage(PdfTranslationStage::Normalizing, "OCR document normalized");
        Ok(document)
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfTranslationError {
    pub stage: PdfTranslationStage,
    pub message: String,
    pub retryable: bool,
}

impl PdfTranslationError {
    fn cancelled(stage: PdfTranslationStage) -> Self {
        Self {
            stage,
            message: "PDF translation was cancelled".to_string(),
            retryable: false,
        }
    }

    fn message(stage: PdfTranslationStage, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
            retryable: false,
        }
    }

    fn stage(stage: PdfTranslationStage, context: &str, error: impl fmt::Display) -> Self {
        Self {
            stage,
            message: format!("{context}: {error}"),
            retryable: false,
        }
    }
}

impl fmt::Display for PdfTranslationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for PdfTranslationError {}

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
) -> PdfTranslationArtifactManifest {
    PdfTranslationArtifactManifest {
        version: ARTIFACT_VERSION,
        pipeline_version: PIPELINE_VERSION.to_string(),
        fingerprint: fingerprint.to_string(),
        source_path: request.source_path.to_string_lossy().to_string(),
        output_path: request.output_path.to_string_lossy().to_string(),
        target_language: request.target_language.as_str().to_string(),
        ocr_mode: settings.ocr.mode.as_str().to_string(),
        ocr_provider: if translated.document.metadata.provider == "paddle" {
            "paddle".to_string()
        } else {
            "mupdf".to_string()
        },
        translation_provider: settings.translation.active_provider.as_str().to_string(),
        translated_unit_count: translated.units.len(),
        job_directory: job_directory.to_string_lossy().to_string(),
        geometry: layout
            .pages
            .iter()
            .flat_map(|page| {
                page.items.iter().map(|item| ArtifactGeometry {
                    block_id: item.block_id.clone(),
                    page_index: item.page_index,
                    source_bbox: item.bbox,
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
    let mut extracted_text = String::new();
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
        extracted_text.push_str(&text);
    }
    let expected = translated
        .units
        .iter()
        .filter(|unit| unit.translated_text.chars().count() >= 4)
        .take(8)
        .collect::<Vec<_>>();
    let found = expected
        .iter()
        .filter(|unit| {
            let sample = unit
                .translated_text
                .split_whitespace()
                .take(5)
                .collect::<Vec<_>>()
                .join(" ");
            !sample.is_empty() && extracted_text.contains(&sample)
        })
        .count();
    if !expected.is_empty() && found == 0 {
        return Err(PdfTranslationError::message(
            PdfTranslationStage::Validating,
            "Translated PDF does not contain searchable translated text",
        ));
    }
    Ok(())
}

fn request_fingerprint(
    settings: &Settings,
    request: &PdfTranslationRequest,
) -> Result<String, PdfTranslationError> {
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
    settings.ocr.endpoint.hash(&mut hasher);
    settings.ocr.model.hash(&mut hasher);
    settings.translation.active_provider.hash(&mut hasher);
    if settings.translation.active_provider == glyphst_settings::TranslationProvider::Ai {
        let resolved = settings.ai.resolved_translation();
        resolved.provider.as_str().hash(&mut hasher);
        resolved.model.hash(&mut hasher);
    }
    Ok(format!("{:016x}", hasher.finish()))
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
    fs::rename(&temporary, destination).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        PdfTranslationError::stage(
            PdfTranslationStage::Validating,
            "Could not publish the translated PDF",
            error,
        )
    })
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
    fn fingerprint_does_not_include_ocr_or_translation_secrets() {
        let mut settings = Settings::default();
        settings.ocr.token = "paddle-secret".to_string();
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
        assert!(!fingerprint.contains("google-secret"));
    }
}
