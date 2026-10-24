mod cancellation;
mod composition;
mod document;
mod layout;
mod native;
mod paddle;
mod policy;
mod progress;
mod reference;
mod scope;
mod toc;
mod translate;
mod typst;
mod unlimited;

pub use cancellation::{CancellationToken, TranslationCancelled};
pub use composition::{
    PdfComposer, PdfCompositionError, PdfCompositionReport, compose_side_by_side,
    sanitize_source_pdf,
};
pub use document::{
    BaselinePosition, Block, BlockContent, BlockKind, BlockRole, BlockStructure, BoundingBox,
    ContinuationHint, ContinuationScope, DocumentMetadata, DocumentV1, FontFamilyClass,
    InlineObject, InlineObjectPart, Line, Page, PageGeometry, PolicyDecisionSource,
    ProtectedRegion, ProtectedRegionKind, Segment, SegmentKind, SourceProvenance, StructureKind,
    TableOfContentsEntry, TableOfContentsLeader, TextFlow, TextStyle, TranslationPolicy,
    inline_object_marker, strip_inline_object_markers,
};
pub use layout::{
    FontWeight, LayoutDocument, LayoutItem, LayoutOptions, LayoutPage, LayoutWarning,
    OverflowPolicy, TableOfContentsLayout, build_layout, build_layout_with_options,
    source_text_regions, split_text_for_layout_boxes,
};
pub use native::{
    OcrFusionDecision, OcrFusionPageReport, OcrFusionRelation, OcrFusionReport, OcrFusionResult,
    PdfInspection, PdfPageProfile, extract_native_document, fuse_native_with_ocr,
    fuse_native_with_ocr_report, inspect_pdf, reconcile_ocr_geometry,
};
pub use paddle::{
    PaddleClient, PaddleClientConfig, PaddleError, PaddleErrorCategory, PaddleJobProgress,
    PaddleProbeResult, normalize_paddle_payload, reconcile_provider_continuations,
};
pub use policy::{
    POLICY_REVIEW_CONFIDENCE_THRESHOLD, PolicyReviewBlock, PolicyReviewContextBlock,
    PolicyReviewDecision, PolicyReviewError, PolicyReviewRequest, TranslationPolicyReviewer,
    enforce_protected_regions, review_translation_policies,
};
pub use progress::{
    PdfTranslationProgress, PdfTranslationStage, ProgressTracker, TranslationProgressSink,
};
pub use reference::{ReferenceClassification, annotate_reference_sections};
pub use scope::{ScopeView, TranslationScope};
pub use translate::{
    ProtectedText, ProtectedToken, ProtectedTokenKind, ScopedRun, StyleDefinition, StyledRun,
    TranslatedDocument, TranslatedUnit, TranslationBackend, TranslationBackendError,
    TranslationDocumentError, TranslationErrorCategory, TranslationFailureDiagnostic,
    TranslationFailurePhase, TranslationQueueStats, TranslationRunOutcome, TranslationRunProgress,
    TranslationRunSnapshot, TranslationUnit, TranslationUnitOutput, document_translation_units,
    protect_text, translate_document, translate_document_partial_resuming,
    translate_document_resuming, translate_document_scoped,
};
pub use typst::{
    TypstCompiler, TypstCompilerConfig, TypstError, TypstErrorKind, TypstExecutionMode,
    TypstOutput, generate_translated_pdf_source, generate_typst_source,
    resolve_translation_assets_dir, run_typst_worker_if_requested,
};
pub use unlimited::{
    UnlimitedClient, UnlimitedClientConfig, UnlimitedError, UnlimitedErrorCategory,
    UnlimitedJobProgress, UnlimitedOcrResult, UnlimitedProbeResult, normalize_unlimited_payload,
};
