mod cancellation;
mod composition;
mod document;
mod job;
mod layout;
mod native;
mod paddle;
mod progress;
mod translate;
mod typst;

pub use cancellation::{CancellationToken, TranslationCancelled};
pub use composition::{
    PdfComposer, PdfCompositionError, PdfCompositionReport, sanitize_source_pdf,
};
pub use document::{
    Block, BlockContent, BlockKind, BlockRole, BoundingBox, ContinuationHint, ContinuationScope,
    DocumentMetadata, DocumentV1, Line, Page, PageGeometry, Segment, SegmentKind, SourceProvenance,
    TextStyle, TranslationPolicy,
};
pub use job::{
    ArtifactGeometry, PdfTranslationArtifactManifest, PdfTranslationError, PdfTranslationRequest,
    PdfTranslationResult, PdfTranslationService,
};
pub use layout::{
    FontWeight, LayoutDocument, LayoutItem, LayoutPage, LayoutWarning, build_layout,
    split_text_for_layout_boxes,
};
pub use native::{
    PdfInspection, PdfPageProfile, extract_native_document, inspect_pdf, reconcile_ocr_geometry,
};
pub use paddle::{
    PaddleClient, PaddleClientConfig, PaddleError, PaddleErrorCategory, PaddleJobProgress,
    PaddleProbeResult, normalize_paddle_payload,
};
pub use progress::{
    PdfTranslationProgress, PdfTranslationStage, ProgressTracker, TranslationProgressSink,
};
pub use translate::{
    GlyphstTranslationBackend, ProtectedText, ProtectedToken, ProtectedTokenKind,
    TranslatedDocument, TranslatedUnit, TranslationBackend, TranslationBackendError,
    TranslationDocumentError, TranslationUnit, TranslationUnitOutput, protect_text,
    translate_document,
};
pub use typst::{
    TypstCompiler, TypstCompilerConfig, TypstError, TypstOutput, generate_translated_pdf_source,
    generate_typst_source, resolve_translation_assets_dir,
};
