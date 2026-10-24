mod backend;
mod job;
mod local_ocr;
mod ocr_probe;
mod reviewer;

use glyphst_settings::{ModelRef, OcrSettings, PdfContentReviewMode, ResolvedAiConfig, Settings};
use glyphst_translation::TranslationExecutionConfig;

/// Immutable execution inputs captured when a PDF translation is submitted.
/// Background jobs never retain the application's complete settings aggregate.
#[derive(Clone, Debug)]
pub struct PdfTranslationConfig {
    pub(crate) translation: TranslationExecutionConfig,
    pub(crate) ocr: OcrSettings,
    pub(crate) content_review: PdfContentReviewConfig,
    /// Recognise again pages whose recognition failed before, instead of
    /// reusing the document prepared then.
    pub(crate) retry_failed_pages: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct PdfContentReviewConfig {
    pub(crate) mode: PdfContentReviewMode,
    pub(crate) model: Option<ModelRef>,
    pub(crate) resolved: Option<ResolvedAiConfig>,
}

impl PdfTranslationConfig {
    pub fn from_settings(settings: &Settings) -> Self {
        let review_model = settings.translation.content_review.model.clone();
        let resolved_review_model = review_model
            .as_ref()
            .and_then(|model| settings.ai.resolved_model(model));
        Self {
            translation: TranslationExecutionConfig::from_settings(settings),
            ocr: settings.ocr.clone(),
            content_review: PdfContentReviewConfig {
                mode: settings.translation.content_review.mode,
                model: review_model,
                resolved: resolved_review_model,
            },
            retry_failed_pages: false,
        }
    }

    /// The same, recognising again the pages that failed last time.
    pub fn retrying_failed_pages(mut self) -> Self {
        self.retry_failed_pages = true;
        self
    }
}

impl From<&Settings> for PdfTranslationConfig {
    fn from(settings: &Settings) -> Self {
        Self::from_settings(settings)
    }
}

pub use glyphst_pdf_translation::*;
pub use job::{
    ArtifactGeometry, ExportLayout, PageFailureCode, PdfTranslationArtifactManifest,
    PdfTranslationError, PdfTranslationExportRequest, PdfTranslationHoverRegion,
    PdfTranslationHoverRegionKind, PdfTranslationPageFailure, PdfTranslationPageGlyph,
    PdfTranslationPagePoint, PdfTranslationPagePreview, PdfTranslationPageQuad,
    PdfTranslationPageTextBlock, PdfTranslationPageTextLayer, PdfTranslationPageTextLine,
    PdfTranslationRequest, PdfTranslationRestoredSession, PdfTranslationResult,
    PdfTranslationService, PdfTranslationSessionCacheKey, PdfTranslationSessionRequest,
    PdfTranslationSessionResult, PdfTranslationSessionUpdate, PdfTranslationWarning,
};
pub use ocr_probe::{OcrProbeError, test_ocr_connection};
