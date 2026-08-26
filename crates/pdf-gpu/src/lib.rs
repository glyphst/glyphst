//! Glyphst application adapter for the standalone retained GPU PDF engine.
//!
//! This crate is intentionally outside `glyphst-pdf-engine`: it is the narrow
//! boundary where GPUI's same-device WGPU handle is translated into the
//! engine-owned renderer and an ordered external-texture primitive.

#![forbid(unsafe_code)]

use anyhow::{Context as _, Result, anyhow, bail};
use glyphst_core::DocumentRotation;
use glyphst_pdf::DocumentSnapshot;
use glyphst_pdf_engine::{
    Action as EngineAction, CompileError, CompileOptions, CompiledPage,
    Destination as EngineDestination, DestinationView as EngineDestinationView, DiagnosticSeverity,
    Document, DocumentSource, Engine, LayerConfiguration, LinkHighlightMode, OpenOptions,
    OutlineItem as EngineOutlineItem, PageIndex, Password, Rect, SupportDiagnostic, Transform,
};
pub use glyphst_pdf_engine::{CancellationToken, FeatureId, Permissions as GpuPdfPermissions};
use glyphst_pdf_render_wgpu::{
    ANALYTIC_TILE_SIZE, DeviceToken, GpuPage, GpuRenderer, IncrementalRenderState, PrepareError,
    RenderConfig, RenderError, RenderRequest, RenderSupportIssue, RendererInitError,
};
pub use glyphst_pdf_render_wgpu::{
    PatchLodPreparationStats as GpuPatchLodPreparationStats, PatchLodStatus as GpuPatchLodStatus,
    PreparationStats as GpuPreparationStats, RenderStats as GpuRenderStats,
};
use gpui::{ExternalGpuContext, ExternalGpuDeviceToken, ExternalTextureHandle};
use gpui_wgpu::{WgpuExternalContext, wgpu};
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
};

const MAX_COMPILED_PAGE_CACHE_ENTRIES: usize = 8;
const MAX_CONTENT_BOUNDS_CACHE_ENTRIES: usize = 128;

/// Parser/interpreter or WGPU capability that requires whole-page fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuFallbackDiagnostic {
    pub feature: FeatureId,
    pub code: String,
    pub message: String,
    pub source: GpuFallbackSource,
}

/// Stage that rejected a complete page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuFallbackSource {
    Frontend,
    Renderer,
    Application,
}

impl GpuFallbackDiagnostic {
    /// The reader cannot prove that its Link hit regions match the native
    /// engine's canonical interaction geometry.
    pub fn application_link_geometry_mismatch() -> Self {
        Self {
            feature: FeatureId::Links,
            code: "application-link-geometry-mismatch".into(),
            message: "native Link annotation rectangles do not map one-for-one to the reader Links"
                .into(),
            source: GpuFallbackSource::Application,
        }
    }
}

/// Link press effects implemented by the same-device presentation adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuLinkHighlightMode {
    None,
    Invert,
}

/// Safe action executed for a Link on a native page.
#[derive(Clone, Debug, PartialEq)]
pub enum GpuLinkAction {
    GoTo { page: u32, view: GpuDestinationView },
    Uri(Arc<str>),
    UriMap(GpuUriMapAction),
    Named(GpuNamedAction),
    Sequence(Arc<[Self]>),
    NoOp { code: Arc<str>, message: Arc<str> },
}

impl GpuLinkAction {
    /// Whether exact execution first needs a lazy target-page content query.
    #[must_use]
    pub fn requires_content_bounds(&self) -> bool {
        match self {
            Self::GoTo { view, .. } => view.requires_content_bounds(),
            Self::Sequence(actions) => actions.iter().any(Self::requires_content_bounds),
            Self::Uri(_) | Self::UriMap(_) | Self::Named(_) | Self::NoOp { .. } => false,
        }
    }
}

/// Failure while resolving a lazy native destination action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GpuActionResolutionError {
    Cancelled,
    Fallback(GpuFallbackDiagnostic),
}

/// A URI action resolved with the canonical click position of its Link.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuUriMapAction {
    inner: glyphst_pdf_engine::UriMapAction,
}

impl GpuUriMapAction {
    /// The validated URI before PDF map coordinates are appended.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.inner.uri
    }

    /// Appends the nearest integer annotation-local coordinates for one
    /// canonical page click. Non-finite input or mapping fails closed.
    #[must_use]
    pub fn resolve_at(&self, x: f64, y: f64) -> Option<String> {
        self.inner
            .resolve_at(glyphst_pdf_engine::Point::new(x, y).ok()?)
    }
}

/// Canonical viewport operation for an explicit native-page destination.
#[derive(Clone, Debug, PartialEq)]
pub enum GpuDestinationView {
    Xyz {
        x: Option<f64>,
        y: Option<f64>,
        zoom: Option<f32>,
    },
    FitWidth {
        top: Option<f64>,
    },
    FitHeight {
        left: Option<f64>,
    },
    FitPage,
    FitBoundingBox {
        content_bounds: Option<[f64; 4]>,
    },
    FitBoundingBoxWidth {
        top: Option<f64>,
        content_bounds: Option<[f64; 4]>,
    },
    FitBoundingBoxHeight {
        left: Option<f64>,
        content_bounds: Option<[f64; 4]>,
    },
    FitRectangle([f64; 4]),
}

impl GpuDestinationView {
    fn requires_content_bounds(&self) -> bool {
        matches!(
            self,
            Self::FitBoundingBox {
                content_bounds: None
            } | Self::FitBoundingBoxWidth {
                content_bounds: None,
                ..
            } | Self::FitBoundingBoxHeight {
                content_bounds: None,
                ..
            }
        )
    }
}

/// Standard safe `/Named` actions defined for document navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuNamedAction {
    NextPage,
    PreviousPage,
    FirstPage,
    LastPage,
}

/// Fully owned table-of-contents item installed only after document-level
/// native navigation preflight and application action conversion both pass.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuOutlineItem {
    pub title: Arc<str>,
    pub action: Option<GpuLinkAction>,
    pub children: Arc<[Self]>,
    pub initially_open: bool,
    pub color: Option<[f32; 3]>,
    pub italic: bool,
    pub bold: bool,
}

/// Result of attempting to replace the host reader's document navigation.
/// This is intentionally independent from page routing.
#[derive(Clone, Debug)]
pub enum GpuNavigationRoute {
    Ready(Arc<[GpuOutlineItem]>),
    Fallback(Arc<[GpuFallbackDiagnostic]>),
}

/// Engine-owned Link interaction geometry in canonical page coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuLinkInteraction {
    /// Canonical annotation `/Rect`; PDF invert highlighting applies here.
    pub annotation_bounds: [f64; 4],
    /// Exact ordered quadrilaterals `[x0, y0, ... x3, y3]` for hit testing.
    pub regions: Arc<[[f64; 8]]>,
    pub highlight_mode: GpuLinkHighlightMode,
    pub action: GpuLinkAction,
}

impl GpuLinkInteraction {
    /// Whether canonical page coordinates lie in any exact activation quad.
    #[must_use]
    pub fn contains_point(&self, x: f64, y: f64) -> bool {
        x.is_finite()
            && y.is_finite()
            && self
                .regions
                .iter()
                .any(|region| point_in_quad(*region, x, y))
    }
}

fn point_in_quad(region: [f64; 8], x: f64, y: f64) -> bool {
    let points = [
        [region[0], region[1]],
        [region[2], region[3]],
        [region[4], region[5]],
        [region[6], region[7]],
    ];
    if !points.iter().flatten().all(|value| value.is_finite()) {
        return false;
    }
    let scale = points
        .iter()
        .flatten()
        .copied()
        .chain([x, y])
        .map(f64::abs)
        .fold(1.0_f64, f64::max);
    let points = points.map(|point| [point[0] / scale, point[1] / scale]);
    let x = x / scale;
    let y = y / scale;
    let epsilon = f64::EPSILON * 64.0;
    let mut inside = false;
    for index in 0..4 {
        let first = points[index];
        let second = points[(index + 1) % 4];
        let cross =
            (second[0] - first[0]) * (y - first[1]) - (second[1] - first[1]) * (x - first[0]);
        let dot = (x - first[0]) * (x - second[0]) + (y - first[1]) * (y - second[1]);
        if cross.abs() <= epsilon && dot <= epsilon {
            return true;
        }
        let crosses = (first[1] > y) != (second[1] > y);
        if crosses {
            let crossing_x =
                (second[0] - first[0]) * (y - first[1]) / (second[1] - first[1]) + first[0];
            inside ^= x < crossing_x;
        }
    }
    inside
}

impl From<&SupportDiagnostic> for GpuFallbackDiagnostic {
    fn from(diagnostic: &SupportDiagnostic) -> Self {
        Self {
            feature: diagnostic.feature,
            code: diagnostic.code.clone(),
            message: diagnostic.message.clone(),
            source: GpuFallbackSource::Frontend,
        }
    }
}

impl From<&RenderSupportIssue> for GpuFallbackDiagnostic {
    fn from(issue: &RenderSupportIssue) -> Self {
        Self {
            feature: issue.feature,
            code: issue.code.clone(),
            message: issue.message.clone(),
            source: GpuFallbackSource::Renderer,
        }
    }
}

/// Result of attempting to route a page to the native GPU engine.
#[derive(Clone)]
pub enum GpuPageRoute {
    Ready(Arc<PreparedGpuPage>),
    Fallback(Arc<[GpuFallbackDiagnostic]>),
}

impl fmt::Debug for GpuPageRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready(page) => formatter.debug_tuple("Ready").field(page).finish(),
            Self::Fallback(diagnostics) => formatter
                .debug_tuple("Fallback")
                .field(diagnostics)
                .finish(),
        }
    }
}

/// Open standalone PDF document plus a shared owned-page compilation cache.
#[derive(Clone)]
pub struct GpuPdfDocument {
    inner: Arc<GpuPdfDocumentInner>,
}

struct GpuPdfDocumentInner {
    document: Document,
    navigation: GpuNavigationRoute,
    compiled: Mutex<CompiledPageCache>,
    content_bounds: Mutex<ContentBoundsCache>,
}

#[derive(Default)]
struct CompiledPageCache {
    entries: HashMap<PageIndex, CachedCompiledPage>,
    clock: u64,
}

struct CachedCompiledPage {
    page: Arc<CompiledPage>,
    last_used: u64,
}

#[derive(Default)]
struct ContentBoundsCache {
    entries: HashMap<PageIndex, CachedContentBounds>,
    clock: u64,
}

struct CachedContentBounds {
    result: std::result::Result<[f64; 4], GpuFallbackDiagnostic>,
    last_used: u64,
}

impl ContentBoundsCache {
    fn get(
        &mut self,
        index: PageIndex,
    ) -> Option<std::result::Result<[f64; 4], GpuFallbackDiagnostic>> {
        self.clock = self.clock.wrapping_add(1).max(1);
        let entry = self.entries.get_mut(&index)?;
        entry.last_used = self.clock;
        Some(entry.result.clone())
    }

    fn insert(
        &mut self,
        index: PageIndex,
        result: std::result::Result<[f64; 4], GpuFallbackDiagnostic>,
    ) -> std::result::Result<[f64; 4], GpuFallbackDiagnostic> {
        self.clock = self.clock.wrapping_add(1).max(1);
        if let Some(entry) = self.entries.get_mut(&index) {
            entry.last_used = self.clock;
            return entry.result.clone();
        }
        if self.entries.len() >= MAX_CONTENT_BOUNDS_CACHE_ENTRIES
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(index, _)| *index)
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(
            index,
            CachedContentBounds {
                result: result.clone(),
                last_used: self.clock,
            },
        );
        result
    }
}

impl CompiledPageCache {
    fn get(&mut self, index: PageIndex) -> Option<Arc<CompiledPage>> {
        self.clock = self.clock.wrapping_add(1).max(1);
        let entry = self.entries.get_mut(&index)?;
        entry.last_used = self.clock;
        Some(Arc::clone(&entry.page))
    }

    fn insert(&mut self, index: PageIndex, page: Arc<CompiledPage>) -> Arc<CompiledPage> {
        self.clock = self.clock.wrapping_add(1).max(1);
        if let Some(entry) = self.entries.get_mut(&index) {
            entry.last_used = self.clock;
            return Arc::clone(&entry.page);
        }
        if self.entries.len() >= MAX_COMPILED_PAGE_CACHE_ENTRIES
            && let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(index, _)| *index)
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(
            index,
            CachedCompiledPage {
                page: Arc::clone(&page),
                last_used: self.clock,
            },
        );
        page
    }
}

impl fmt::Debug for GpuPdfDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuPdfDocument")
            .field("document", &self.inner.document)
            .finish_non_exhaustive()
    }
}

impl GpuPdfDocument {
    /// Open the immutable snapshot without duplicating its source allocation.
    pub fn open(snapshot: &DocumentSnapshot) -> Result<Self> {
        let options = OpenOptions {
            password: snapshot
                .password()
                .map(|password| Password::new(password.expose_secret())),
            ..OpenOptions::default()
        };
        let document = Engine::open(DocumentSource::from_bytes(snapshot.shared_bytes()), options)
            .context("failed to open PDF with the native GPU engine")?;
        let navigation = compiled_document_navigation(&document);
        Ok(Self {
            inner: Arc::new(GpuPdfDocumentInner {
                document,
                navigation,
                compiled: Mutex::new(CompiledPageCache::default()),
                content_bounds: Mutex::new(ContentBoundsCache::default()),
            }),
        })
    }

    /// Number of checked pages in the native engine.
    pub fn page_count(&self) -> u32 {
        self.inner.document.page_count().get()
    }

    /// Whether the source uses a PDF encryption security handler.
    #[must_use]
    pub fn is_encrypted(&self) -> bool {
        self.inner.document.is_encrypted()
    }

    /// Validated permissions for the authenticated owner or user password.
    #[must_use]
    pub fn permissions(&self) -> GpuPdfPermissions {
        self.inner.document.permissions()
    }

    /// Current bounded CPU semantic-scene cache occupancy.
    pub fn compiled_cache_entries(&self) -> usize {
        self.inner
            .compiled
            .lock()
            .map(|cache| cache.entries.len())
            .unwrap_or_default()
    }

    pub const fn compiled_cache_limit() -> usize {
        MAX_COMPILED_PAGE_CACHE_ENTRIES
    }

    /// Current bounded lazy destination-bound cache occupancy.
    pub fn content_bounds_cache_entries(&self) -> usize {
        self.inner
            .content_bounds
            .lock()
            .map(|cache| cache.entries.len())
            .unwrap_or_default()
    }

    pub const fn content_bounds_cache_limit() -> usize {
        MAX_CONTENT_BOUNDS_CACHE_ENTRIES
    }

    /// Native outline route for this document. A navigation fallback does not
    /// affect eligibility of any page for native GPU rendering.
    pub fn navigation_route(&self) -> GpuNavigationRoute {
        self.inner.navigation.clone()
    }

    /// Conservative frontend-only preflight, suitable before a GPU context exists.
    pub fn frontend_diagnostics(&self, page: u32) -> Result<Vec<GpuFallbackDiagnostic>> {
        let page = self.checked_page(page)?;
        Ok(self
            .inner
            .document
            .preflight_page(page, &LayerConfiguration::default())
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Unsupported)
            .map(GpuFallbackDiagnostic::from)
            .collect())
    }

    /// Compile one complete page into the engine-owned semantic graph.
    pub fn compile_page(
        &self,
        page: u32,
        cancellation: &CancellationToken,
    ) -> Result<Arc<CompiledPage>> {
        let page = self.checked_page(page)?;
        if let Some(compiled) = self
            .inner
            .compiled
            .lock()
            .map_err(|_| anyhow!("native PDF page cache is poisoned"))?
            .get(page)
        {
            return Ok(compiled);
        }

        let compiled = self
            .inner
            .document
            .compile_page(page, CompileOptions::default(), cancellation)
            .with_context(|| format!("failed to compile native PDF page {}", page.get() + 1))?;
        let compiled = self
            .inner
            .compiled
            .lock()
            .map_err(|_| anyhow!("native PDF page cache is poisoned"))?
            .insert(page, compiled);
        Ok(compiled)
    }

    /// Resolves every FitB-family destination in an action tree while
    /// preserving exact execution order. Only referenced target pages are
    /// interpreted, and their results enter a bounded document cache.
    pub fn resolve_link_action(
        &self,
        action: &GpuLinkAction,
        cancellation: &CancellationToken,
    ) -> std::result::Result<GpuLinkAction, GpuActionResolutionError> {
        if cancellation.is_cancelled() {
            return Err(GpuActionResolutionError::Cancelled);
        }
        match action {
            GpuLinkAction::GoTo { page, view } => Ok(GpuLinkAction::GoTo {
                page: *page,
                view: self.resolve_destination_view(*page, view, cancellation)?,
            }),
            GpuLinkAction::Sequence(actions) => actions
                .iter()
                .map(|action| self.resolve_link_action(action, cancellation))
                .collect::<std::result::Result<Vec<_>, _>>()
                .map(Arc::from)
                .map(GpuLinkAction::Sequence),
            GpuLinkAction::Uri(uri) => Ok(GpuLinkAction::Uri(Arc::clone(uri))),
            GpuLinkAction::UriMap(action) => Ok(GpuLinkAction::UriMap(action.clone())),
            GpuLinkAction::Named(action) => Ok(GpuLinkAction::Named(*action)),
            GpuLinkAction::NoOp { code, message } => Ok(GpuLinkAction::NoOp {
                code: Arc::clone(code),
                message: Arc::clone(message),
            }),
        }
    }

    fn resolve_destination_view(
        &self,
        page: u32,
        view: &GpuDestinationView,
        cancellation: &CancellationToken,
    ) -> std::result::Result<GpuDestinationView, GpuActionResolutionError> {
        let content_bounds = match view {
            GpuDestinationView::FitBoundingBox {
                content_bounds: None,
            }
            | GpuDestinationView::FitBoundingBoxWidth {
                content_bounds: None,
                ..
            }
            | GpuDestinationView::FitBoundingBoxHeight {
                content_bounds: None,
                ..
            } => Some(self.resolve_content_bounds(page, cancellation)?),
            _ => None,
        };
        Ok(match view {
            GpuDestinationView::FitBoundingBox {
                content_bounds: None,
            } => {
                let Some(bounds) = content_bounds else {
                    return Err(missing_content_bounds_resolution());
                };
                require_content_extent(bounds, true, true)?;
                GpuDestinationView::FitBoundingBox {
                    content_bounds: Some(bounds),
                }
            }
            GpuDestinationView::FitBoundingBoxWidth {
                top,
                content_bounds: None,
            } => {
                let Some(bounds) = content_bounds else {
                    return Err(missing_content_bounds_resolution());
                };
                require_content_extent(bounds, true, false)?;
                GpuDestinationView::FitBoundingBoxWidth {
                    top: *top,
                    content_bounds: Some(bounds),
                }
            }
            GpuDestinationView::FitBoundingBoxHeight {
                left,
                content_bounds: None,
            } => {
                let Some(bounds) = content_bounds else {
                    return Err(missing_content_bounds_resolution());
                };
                require_content_extent(bounds, false, true)?;
                GpuDestinationView::FitBoundingBoxHeight {
                    left: *left,
                    content_bounds: Some(bounds),
                }
            }
            _ => view.clone(),
        })
    }

    fn resolve_content_bounds(
        &self,
        page: u32,
        cancellation: &CancellationToken,
    ) -> std::result::Result<[f64; 4], GpuActionResolutionError> {
        let page = self.checked_page(page).map_err(|error| {
            GpuActionResolutionError::Fallback(application_link_action_fallback(
                "application-destination-page-out-of-bounds",
                &error.to_string(),
            ))
        })?;
        if let Some(result) = self
            .inner
            .content_bounds
            .lock()
            .map_err(|_| {
                GpuActionResolutionError::Fallback(application_link_action_fallback(
                    "application-content-bounds-cache-poisoned",
                    "the native destination content-bound cache is poisoned",
                ))
            })?
            .get(page)
        {
            return result.map_err(GpuActionResolutionError::Fallback);
        }

        let result = match self.inner.document.content_bounds(
            page,
            &LayerConfiguration::default(),
            cancellation,
        ) {
            Ok(Some(bounds)) => application_content_bounds(bounds),
            Ok(None) => Err(application_link_action_fallback(
                "application-destination-content-bounds-empty",
                "a FitB-family destination targets a page without paintable content",
            )),
            Err(CompileError::Cancelled) => return Err(GpuActionResolutionError::Cancelled),
            Err(CompileError::Unsupported { feature, message }) => Err(GpuFallbackDiagnostic {
                feature,
                code: "application-destination-content-bounds-unsupported".into(),
                message,
                source: GpuFallbackSource::Frontend,
            }),
            Err(error) => Err(GpuFallbackDiagnostic {
                feature: FeatureId::Links,
                code: "application-destination-content-bounds-failed".into(),
                message: error.to_string(),
                source: GpuFallbackSource::Frontend,
            }),
        };
        self.inner
            .content_bounds
            .lock()
            .map_err(|_| {
                GpuActionResolutionError::Fallback(application_link_action_fallback(
                    "application-content-bounds-cache-poisoned",
                    "the native destination content-bound cache is poisoned",
                ))
            })?
            .insert(page, result)
            .map_err(GpuActionResolutionError::Fallback)
    }

    fn checked_page(&self, page: u32) -> Result<PageIndex> {
        let page = PageIndex::new(page);
        if !self.inner.document.page_count().contains(page) {
            bail!("native PDF page index {} is out of bounds", page.get());
        }
        Ok(page)
    }
}

fn compiled_document_navigation(document: &Document) -> GpuNavigationRoute {
    let diagnostics = document
        .navigation_support()
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Unsupported)
        .map(GpuFallbackDiagnostic::from)
        .collect::<Vec<_>>();
    if !diagnostics.is_empty() {
        return GpuNavigationRoute::Fallback(diagnostics.into());
    }

    match compiled_outline_items(document.outline()) {
        Ok(items) => GpuNavigationRoute::Ready(items),
        Err(diagnostic) => GpuNavigationRoute::Fallback(Arc::from([diagnostic])),
    }
}

fn compiled_outline_items(
    items: &[EngineOutlineItem],
) -> std::result::Result<Arc<[GpuOutlineItem]>, GpuFallbackDiagnostic> {
    items
        .iter()
        .map(|item| {
            let action = item
                .action
                .as_ref()
                .map(compiled_outline_action)
                .transpose()?;
            Ok(GpuOutlineItem {
                title: Arc::from(item.title.as_str()),
                action,
                children: compiled_outline_items(&item.children)?,
                initially_open: item.initially_open,
                color: item.color,
                italic: item.italic,
                bold: item.bold,
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map(Arc::from)
}

fn compiled_outline_action(
    action: &EngineAction,
) -> std::result::Result<GpuLinkAction, GpuFallbackDiagnostic> {
    compiled_link_action(action).map_err(|diagnostic| GpuFallbackDiagnostic {
        feature: FeatureId::Outlines,
        code: match diagnostic.code.as_str() {
            "application-link-action-sequence-empty" => {
                "application-outline-action-sequence-empty".into()
            }
            "application-link-action-unsupported" => {
                "application-outline-action-unsupported".into()
            }
            _ => diagnostic.code,
        },
        message: diagnostic
            .message
            .replace("a native Link", "an outline item")
            .replace("the native Link", "the outline item"),
        source: diagnostic.source,
    })
}

/// One complete page retained on a specific GPUI device generation.
pub struct PreparedGpuPage {
    compiled: Arc<CompiledPage>,
    gpu: Arc<GpuPage>,
    link_interactions: Arc<[GpuLinkInteraction]>,
}

impl fmt::Debug for PreparedGpuPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedGpuPage")
            .field("compiled", &self.compiled)
            .field("gpu", &self.gpu)
            .finish()
    }
}

impl PreparedGpuPage {
    pub fn compiled(&self) -> &Arc<CompiledPage> {
        &self.compiled
    }

    pub fn device_token(&self) -> ExternalGpuDeviceToken {
        let token = self.gpu.device_token();
        ExternalGpuDeviceToken::new(token.identity, token.generation)
    }

    pub fn preparation_stats(&self) -> GpuPreparationStats {
        self.gpu.preparation_stats()
    }

    pub fn patch_lod_preparation_stats(&self) -> GpuPatchLodPreparationStats {
        self.gpu.patch_lod_preparation_stats()
    }

    /// Exact Link press regions that must be honored by the presentation UI.
    pub fn link_interactions(&self) -> &[GpuLinkInteraction] {
        &self.link_interactions
    }
}

fn compiled_link_interactions(
    page: &CompiledPage,
) -> std::result::Result<Arc<[GpuLinkInteraction]>, GpuFallbackDiagnostic> {
    page.links()
        .iter()
        .map(|link| {
            let highlight_mode = match link.highlight_mode.as_ref() {
                Some(LinkHighlightMode::None) => GpuLinkHighlightMode::None,
                Some(LinkHighlightMode::Invert) => GpuLinkHighlightMode::Invert,
                Some(LinkHighlightMode::Outline | LinkHighlightMode::Push)
                | Some(LinkHighlightMode::Other(_))
                | None => {
                    return Err(GpuFallbackDiagnostic {
                        feature: FeatureId::Links,
                        code: "application-link-highlight-unavailable".into(),
                        message: "the native page contains a Link press effect that the presentation adapter cannot execute exactly".into(),
                        source: GpuFallbackSource::Application,
                    });
                }
            };
            let annotation_bounds = [
                link.bounds.x0,
                link.bounds.y0,
                link.bounds.x1,
                link.bounds.y1,
            ];
            let regions = link
                .quads
                .iter()
                .map(|quad| {
                    [
                        quad.p0.x, quad.p0.y, quad.p1.x, quad.p1.y, quad.p2.x, quad.p2.y,
                        quad.p3.x, quad.p3.y,
                    ]
                })
                .collect::<Vec<_>>();
            let action = compiled_link_action(&link.action)?;
            Ok(GpuLinkInteraction {
                annotation_bounds,
                regions: regions.into(),
                highlight_mode,
                action,
            })
        })
        .collect::<std::result::Result<Vec<_>, _>>()
        .map(Arc::from)
}

fn compiled_link_action(
    action: &EngineAction,
) -> std::result::Result<GpuLinkAction, GpuFallbackDiagnostic> {
    match action {
        EngineAction::GoTo(EngineDestination::Page { page, view }) => Ok(GpuLinkAction::GoTo {
            page: page.get(),
            view: compiled_destination_view(view)?,
        }),
        EngineAction::GoTo(EngineDestination::Named(_)) => Err(application_link_action_fallback(
            "application-named-destination-unresolved",
            "a native Link refers to a named destination that the engine has not resolved to an explicit page view",
        )),
        EngineAction::Uri(uri) => Ok(GpuLinkAction::Uri(Arc::from(uri.as_str()))),
        EngineAction::UriMap(action) => Ok(GpuLinkAction::UriMap(GpuUriMapAction {
            inner: action.clone(),
        })),
        EngineAction::Named(name) => {
            let action = match name.as_str() {
                "NextPage" => GpuNamedAction::NextPage,
                "PrevPage" => GpuNamedAction::PreviousPage,
                "FirstPage" => GpuNamedAction::FirstPage,
                "LastPage" => GpuNamedAction::LastPage,
                _ => {
                    return Err(application_link_action_fallback(
                        "application-named-action-unsupported",
                        "a native Link uses a Named action outside the standard page-navigation set",
                    ));
                }
            };
            Ok(GpuLinkAction::Named(action))
        }
        EngineAction::Sequence(actions) if actions.is_empty() => {
            Err(application_link_action_fallback(
                "application-link-action-sequence-empty",
                "a native Link contains an empty action sequence",
            ))
        }
        EngineAction::Sequence(actions) => actions
            .iter()
            .map(compiled_link_action)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map(Arc::from)
            .map(GpuLinkAction::Sequence),
        EngineAction::NoOp { code, message } => Ok(GpuLinkAction::NoOp {
            code: Arc::from(code.as_str()),
            message: Arc::from(message.as_str()),
        }),
        EngineAction::Unsupported { .. } => Err(application_link_action_fallback(
            "application-link-action-unsupported",
            "a native Link action is not safe and executable by the presentation adapter",
        )),
    }
}

fn compiled_destination_view(
    view: &EngineDestinationView,
) -> std::result::Result<GpuDestinationView, GpuFallbackDiagnostic> {
    match view {
        EngineDestinationView::Xyz { x, y, zoom } => Ok(GpuDestinationView::Xyz {
            x: application_destination_coordinate(*x)?,
            y: application_destination_coordinate(*y)?,
            zoom: *zoom,
        }),
        EngineDestinationView::FitWidth { top } => Ok(GpuDestinationView::FitWidth {
            top: application_destination_coordinate(*top)?,
        }),
        EngineDestinationView::FitHeight { left } => Ok(GpuDestinationView::FitHeight {
            left: application_destination_coordinate(*left)?,
        }),
        EngineDestinationView::FitPage => Ok(GpuDestinationView::FitPage),
        EngineDestinationView::FitRectangle(rect) => Ok(GpuDestinationView::FitRectangle([
            required_application_destination_coordinate(rect.x0)?,
            required_application_destination_coordinate(rect.y0)?,
            required_application_destination_coordinate(rect.x1)?,
            required_application_destination_coordinate(rect.y1)?,
        ])),
        EngineDestinationView::FitBoundingBox => Ok(GpuDestinationView::FitBoundingBox {
            content_bounds: None,
        }),
        EngineDestinationView::FitBoundingBoxWidth { top } => {
            Ok(GpuDestinationView::FitBoundingBoxWidth {
                top: application_destination_coordinate(*top)?,
                content_bounds: None,
            })
        }
        EngineDestinationView::FitBoundingBoxHeight { left } => {
            Ok(GpuDestinationView::FitBoundingBoxHeight {
                left: application_destination_coordinate(*left)?,
                content_bounds: None,
            })
        }
    }
}

fn require_content_extent(
    bounds: [f64; 4],
    require_width: bool,
    require_height: bool,
) -> std::result::Result<(), GpuActionResolutionError> {
    let width = bounds[2] - bounds[0];
    let height = bounds[3] - bounds[1];
    if (!require_width || width > 0.0) && (!require_height || height > 0.0) {
        return Ok(());
    }
    Err(GpuActionResolutionError::Fallback(
        application_link_action_fallback(
            "application-destination-content-bounds-degenerate",
            "the target content box has no extent along an axis required by its FitB view",
        ),
    ))
}

fn missing_content_bounds_resolution() -> GpuActionResolutionError {
    GpuActionResolutionError::Fallback(application_link_action_fallback(
        "application-destination-content-bounds-missing",
        "the target content box was not attached to its resolved FitB view",
    ))
}

fn application_content_bounds(
    bounds: glyphst_pdf_engine::Rect,
) -> std::result::Result<[f64; 4], GpuFallbackDiagnostic> {
    Ok([
        required_application_destination_coordinate(bounds.x0)?,
        required_application_destination_coordinate(bounds.y0)?,
        required_application_destination_coordinate(bounds.x1)?,
        required_application_destination_coordinate(bounds.y1)?,
    ])
}

fn application_destination_coordinate(
    coordinate: Option<f64>,
) -> std::result::Result<Option<f64>, GpuFallbackDiagnostic> {
    coordinate
        .map(required_application_destination_coordinate)
        .transpose()
}

fn required_application_destination_coordinate(
    coordinate: f64,
) -> std::result::Result<f64, GpuFallbackDiagnostic> {
    if !(coordinate as f32).is_finite() {
        return Err(application_link_action_fallback(
            "application-destination-coordinate-out-of-range",
            "a native Link destination coordinate exceeds the reader's finite f32 page space",
        ));
    }
    Ok(coordinate)
}

fn application_link_action_fallback(code: &str, message: &str) -> GpuFallbackDiagnostic {
    GpuFallbackDiagnostic {
        feature: FeatureId::Links,
        code: code.into(),
        message: message.into(),
        source: GpuFallbackSource::Application,
    }
}

/// Same-device PDF renderer tied to one GPUI recovery generation.
pub struct GpuPdfRenderer {
    context: WgpuExternalContext,
    renderer: Arc<GpuRenderer>,
    output_format: wgpu::TextureFormat,
}

impl fmt::Debug for GpuPdfRenderer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuPdfRenderer")
            .field("token", &self.context.token())
            .field("output_format", &self.output_format)
            .finish_non_exhaustive()
    }
}

impl GpuPdfRenderer {
    /// Downcast GPUI's runtime-neutral context and initialize the retained renderer.
    pub fn from_gpui(context: &ExternalGpuContext) -> Result<Self> {
        let context = WgpuExternalContext::from_gpui(context)
            .context("the active GPUI window is not backed by WGPU")?
            .clone();
        if context.is_device_lost() {
            bail!("the active GPUI WGPU device is lost");
        }
        let mut config = RenderConfig::default();
        config.max_render_dimension = config
            .max_render_dimension
            .min(context.device().limits().max_texture_dimension_2d);
        let output_format = config.output_format;
        let token = context.token();
        let renderer = GpuRenderer::new(
            Arc::clone(context.device()),
            Arc::clone(context.queue()),
            DeviceToken {
                identity: token.identity(),
                generation: token.generation(),
            },
            config,
        )
        .map_err(renderer_init_error)?;
        Ok(Self {
            context,
            renderer: Arc::new(renderer),
            output_format,
        })
    }

    pub fn token(&self) -> ExternalGpuDeviceToken {
        self.context.token()
    }

    pub fn is_device_lost(&self) -> bool {
        self.context.is_device_lost()
    }

    /// Complete whole-page routing. No GPU resource is returned when either
    /// the frontend or renderer reports an unsupported semantic.
    pub fn prepare_page(
        &self,
        document: &GpuPdfDocument,
        page: u32,
        cancellation: &CancellationToken,
    ) -> Result<GpuPageRoute> {
        let frontend = document.frontend_diagnostics(page)?;
        if !frontend.is_empty() {
            return Ok(GpuPageRoute::Fallback(frontend.into()));
        }

        let compiled = document.compile_page(page, cancellation)?;
        let compiled_diagnostics = compiled
            .support()
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Unsupported)
            .map(GpuFallbackDiagnostic::from)
            .collect::<Vec<_>>();
        if !compiled_diagnostics.is_empty() {
            return Ok(GpuPageRoute::Fallback(compiled_diagnostics.into()));
        }

        let renderer_support = self
            .renderer
            .preflight(compiled.scene())
            .context("failed to plan native PDF page")?;
        if !renderer_support.is_supported() {
            let diagnostics = renderer_support
                .issues()
                .iter()
                .map(GpuFallbackDiagnostic::from)
                .collect::<Vec<_>>();
            return Ok(GpuPageRoute::Fallback(diagnostics.into()));
        }

        let link_interactions = match compiled_link_interactions(&compiled) {
            Ok(interactions) => interactions,
            Err(diagnostic) => return Ok(GpuPageRoute::Fallback(Arc::from([diagnostic]))),
        };
        let gpu = match self.renderer.prepare_interactive(compiled.scene()) {
            Ok(page) => page,
            Err(PrepareError::Unsupported(report)) => {
                let diagnostics = report
                    .issues()
                    .iter()
                    .map(GpuFallbackDiagnostic::from)
                    .collect::<Vec<_>>();
                return Ok(GpuPageRoute::Fallback(diagnostics.into()));
            }
            Err(error) => return Err(error).context("failed to prepare native PDF GPU page"),
        };
        Ok(GpuPageRoute::Ready(Arc::new(PreparedGpuPage {
            compiled,
            gpu: Arc::new(gpu),
            link_interactions,
        })))
    }

    /// Returns whether the exact Type 6/7 mesh slot for this presentation is
    /// already available without allocating GPU resources.
    pub fn patch_lod_status(
        &self,
        page: &PreparedGpuPage,
        request: GpuPaintRequest,
    ) -> Result<GpuPatchLodStatus> {
        if self.is_device_lost() {
            bail!("the active GPUI WGPU device is lost");
        }
        if page.device_token() != self.token() {
            bail!("the prepared PDF page belongs to a stale GPU generation");
        }
        let (_, root_transform) = self.render_root_transform(page, request)?;
        self.renderer
            .patch_lod_status(&page.gpu, root_transform)
            .map_err(render_error)
    }

    /// Builds the exact patch mesh slot needed by `request`. This method is
    /// thread-safe and intended for the document view's cancellable worker.
    pub fn prepare_patch_lods(
        &self,
        page: &PreparedGpuPage,
        request: GpuPaintRequest,
        cancellation: &CancellationToken,
    ) -> Result<GpuPatchLodPreparationStats> {
        if self.is_device_lost() {
            bail!("the active GPUI WGPU device is lost");
        }
        if page.device_token() != self.token() {
            bail!("the prepared PDF page belongs to a stale GPU generation");
        }
        let (_, root_transform) = self.render_root_transform(page, request)?;
        self.renderer
            .prepare_patch_lods(&page.gpu, root_transform, cancellation)
            .map_err(|error| anyhow!(error).context("failed to prepare native PDF patch LOD"))
    }

    fn render_root_transform(
        &self,
        page: &PreparedGpuPage,
        request: GpuPaintRequest,
    ) -> Result<(GpuPageRegion, Transform)> {
        let surface_region = request.surface_region()?;
        let page_transform =
            page_transform(page.gpu.page_bounds(), request.rotation, request.page_size)?;
        let surface_translation = Transform::new([
            1.0,
            0.0,
            0.0,
            1.0,
            -f64::from(surface_region.x),
            -f64::from(surface_region.y),
        ])
        .context("failed to construct native PDF surface translation")?;
        Ok((surface_region, surface_translation.compose(page_transform)))
    }

    /// Render a prepared complete page into a clipped same-device texture.
    ///
    /// `previous` remains untouched and can continue to be presented if this
    /// call fails. A successful render inherits its incremental exactness map
    /// and samples the prior texture only when the engine proves reuse safe.
    pub fn render_page(
        &self,
        page: &PreparedGpuPage,
        request: GpuPaintRequest,
        previous: Option<&GpuPageTexture>,
    ) -> Result<(GpuPageTexture, GpuRenderStats)> {
        if self.is_device_lost() {
            bail!("the active GPUI WGPU device is lost");
        }
        if page.device_token() != self.token() {
            bail!("the prepared PDF page belongs to a stale GPU generation");
        }
        let (surface_region, root_transform) = self.render_root_transform(page, request)?;
        let [width, height] = surface_region.size();
        if width == 0 || height == 0 {
            bail!("native PDF render target must be non-empty");
        }
        let limit = self.context.device().limits().max_texture_dimension_2d;
        if width > limit || height > limit {
            bail!("native PDF render target {width}x{height} exceeds device limit {limit}");
        }

        let mut texture = self.create_texture(surface_region);
        let visible = request.visible_region.clamp(request.page_size)?;
        let visible_local = Rect {
            x0: f64::from(visible.x.saturating_sub(surface_region.x)),
            y0: f64::from(visible.y.saturating_sub(surface_region.y)),
            x1: f64::from(visible.right().saturating_sub(surface_region.x)),
            y1: f64::from(visible.bottom().saturating_sub(surface_region.y)),
        };
        let viewport = root_transform
            .inverse()
            .context("failed to invert native PDF surface transform")?
            .map_rect(visible_local);
        let mut incremental = previous
            .filter(|texture| texture.token == self.token())
            .map(|texture| texture.incremental.clone())
            .unwrap_or_default();
        let stats = self
            .renderer
            .render_incremental(
                &page.gpu,
                &mut incremental,
                previous
                    .filter(|texture| texture.token == self.token())
                    .map(|texture| texture.view.as_ref()),
                RenderRequest {
                    root_transform,
                    viewport,
                    target_size: surface_region.size(),
                    background: request.background,
                    settled: request.settled,
                    generation: request.generation,
                },
                &texture.view,
            )
            .map_err(render_error)?;
        texture.incremental = incremental;
        Ok((texture, stats))
    }

    fn create_texture(&self, region: GpuPageRegion) -> GpuPageTexture {
        let size = region.size();
        let texture = Arc::new(
            self.context
                .device()
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("glyphst native PDF page"),
                    size: wgpu::Extent3d {
                        width: size[0],
                        height: size[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: self.output_format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                }),
        );
        let view = Arc::new(texture.create_view(&wgpu::TextureViewDescriptor::default()));
        let handle = self.context.texture_handle(Arc::clone(&view));
        GpuPageTexture {
            _texture: texture,
            view,
            handle,
            size,
            region,
            token: self.token(),
            incremental: IncrementalRenderState::default(),
        }
    }
}

fn renderer_init_error(error: RendererInitError) -> anyhow::Error {
    anyhow!(error).context("failed to initialize native PDF GPU renderer")
}

fn render_error(error: RenderError) -> anyhow::Error {
    anyhow!(error).context("failed to execute native PDF GPU render")
}

/// A non-empty physical-pixel region in full-page coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuPageRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl GpuPageRegion {
    pub const fn size(self) -> [u32; 2] {
        [self.width, self.height]
    }

    pub const fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }

    pub const fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }

    pub fn byte_len(self) -> u64 {
        u64::from(self.width)
            .saturating_mul(u64::from(self.height))
            .saturating_mul(4)
    }

    fn clamp(self, page_size: [u32; 2]) -> Result<Self> {
        if page_size.contains(&0) {
            bail!("native PDF page target must be non-empty");
        }
        let x0 = self.x.min(page_size[0]);
        let y0 = self.y.min(page_size[1]);
        let x1 = self.right().min(page_size[0]);
        let y1 = self.bottom().min(page_size[1]);
        if x1 <= x0 || y1 <= y0 {
            bail!("native PDF visible region is empty or outside the page");
        }
        Ok(Self {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        })
    }

    fn with_gutter(self, page_size: [u32; 2], gutter: u32) -> Result<Self> {
        let visible = self.clamp(page_size)?;
        let x0 = visible.x.saturating_sub(gutter);
        let y0 = visible.y.saturating_sub(gutter);
        let x1 = visible.right().saturating_add(gutter).min(page_size[0]);
        let y1 = visible.bottom().saturating_add(gutter).min(page_size[1]);
        Ok(Self {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        })
    }
}

/// Parameters for one visible-region render.
#[derive(Clone, Copy, Debug)]
pub struct GpuPaintRequest {
    pub page_size: [u32; 2],
    pub visible_region: GpuPageRegion,
    pub rotation: DocumentRotation,
    pub background: [f64; 4],
    pub settled: bool,
    pub generation: u64,
}

impl GpuPaintRequest {
    pub fn surface_region(self) -> Result<GpuPageRegion> {
        self.visible_region
            .with_gutter(self.page_size, ANALYTIC_TILE_SIZE)
    }
}

/// Reusable render target and GPUI presentation handle.
#[derive(Clone)]
pub struct GpuPageTexture {
    _texture: Arc<wgpu::Texture>,
    view: Arc<wgpu::TextureView>,
    handle: ExternalTextureHandle,
    size: [u32; 2],
    region: GpuPageRegion,
    token: ExternalGpuDeviceToken,
    incremental: IncrementalRenderState,
}

impl fmt::Debug for GpuPageTexture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuPageTexture")
            .field("handle", &self.handle)
            .field("size", &self.size)
            .field("region", &self.region)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl GpuPageTexture {
    pub fn handle(&self) -> &ExternalTextureHandle {
        &self.handle
    }

    pub const fn size(&self) -> [u32; 2] {
        self.size
    }

    pub const fn region(&self) -> GpuPageRegion {
        self.region
    }

    pub fn byte_len(&self) -> u64 {
        self.region.byte_len().saturating_add(
            self.incremental
                .additional_retained_texture_bytes(self.view.as_ref()),
        )
    }

    /// Exact presentation-source bytes retained for future motion frames.
    pub fn retained_source_byte_len(&self) -> u64 {
        self.incremental.retained_texture_bytes()
    }

    pub fn refinement_pending(&self) -> bool {
        self.incremental.refinement_pending()
    }

    pub const fn token(&self) -> ExternalGpuDeviceToken {
        self.token
    }
}

fn page_transform(
    bounds: glyphst_pdf_engine::Rect,
    rotation: DocumentRotation,
    target_size: [u32; 2],
) -> Result<Transform> {
    let width = bounds.width();
    let height = bounds.height();
    if width <= 0.0 || height <= 0.0 {
        bail!("native PDF page has invalid bounds {bounds:?}");
    }
    let (rotated_width, rotated_height) = if rotation.swaps_width_and_height() {
        (height, width)
    } else {
        (width, height)
    };
    let scale_x = f64::from(target_size[0]) / rotated_width;
    let scale_y = f64::from(target_size[1]) / rotated_height;
    // Render keys have integer texture dimensions, so independently rounding
    // their width and height makes the two nominal scale ratios differ by a
    // fraction of a pixel. Keep the engine root an exact uniform similarity
    // and center the resulting (at most rounding-sized) slack in the target
    // instead of introducing a non-uniform page distortion.
    let scale = scale_x.min(scale_y);
    let offset_x = (f64::from(target_size[0]) - rotated_width * scale) * 0.5;
    let offset_y = (f64::from(target_size[1]) - rotated_height * scale) * 0.5;
    let (a, b, c, d, tx, ty) = match rotation.quarter_turns() {
        0 => (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
        1 => (0.0, 1.0, -1.0, 0.0, height, 0.0),
        2 => (-1.0, 0.0, 0.0, -1.0, width, height),
        3 => (0.0, -1.0, 1.0, 0.0, 0.0, width),
        _ => unreachable!("document rotation is normalized"),
    };
    Transform::new([
        a * scale,
        b * scale,
        c * scale,
        d * scale,
        offset_x + (tx - a * bounds.x0 - c * bounds.y0) * scale,
        offset_y + (ty - b * bounds.x0 - d * bounds.y0) * scale,
    ])
    .context("failed to construct native PDF page transform")
}

#[cfg(test)]
mod tests {
    use super::*;
    use glyphst_core::DocumentFormat;

    fn minimal_pdf() -> Arc<[u8]> {
        Arc::from(
            &b"%PDF-1.4\n\
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 200]/Contents 4 0 R>> endobj\n\
4 0 obj <</Length 0>> stream\nendstream endobj\n\
trailer <</Root 1 0 R>>\n%%EOF"[..],
        )
    }

    fn overprint_pdf() -> Arc<[u8]> {
        Arc::from(
            &b"%PDF-1.4\n\
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 200]/Resources<</ExtGState<</GS1 5 0 R>>>>/Contents 4 0 R>> endobj\n\
4 0 obj <</Length 7>> stream\n/GS1 gs\nendstream endobj\n\
5 0 obj <</Type/ExtGState/OP true>> endobj\n\
trailer <</Root 1 0 R>>\n%%EOF"[..],
        )
    }

    fn link_pdf(highlight: &str) -> Arc<[u8]> {
        link_pdf_with_geometry(highlight, "[10 10 50 30]", "")
    }

    fn link_pdf_with_geometry(highlight: &str, rect: &str, quad_points: &str) -> Arc<[u8]> {
        Arc::from(
            format!(
                "%PDF-1.4\n\
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Contents 4 0 R/Annots[5 0 R]>> endobj\n\
4 0 obj <</Length 0>> stream\nendstream endobj\n\
5 0 obj <</Type/Annot/Subtype/Link/Rect{rect}{quad_points}/Border[0 0 0]{highlight}/A<</S/URI/URI(https://example.com)>>>> endobj\n\
trailer <</Root 1 0 R>>\n%%EOF"
            )
            .into_bytes(),
        )
    }

    fn link_pdf_with_action(action: &str) -> Arc<[u8]> {
        link_pdf_with_catalog_and_action("", action)
    }

    fn link_pdf_with_catalog_and_action(catalog_entries: &str, action: &str) -> Arc<[u8]> {
        let target_content = "0 0 1 rg 20 30 40 50 re f\n";
        Arc::from(
            format!(
                "%PDF-1.4\n\
1 0 obj <</Type/Catalog/Pages 2 0 R{catalog_entries}>> endobj\n\
2 0 obj <</Type/Pages/Kids[3 0 R 6 0 R]/Count 2>> endobj\n\
3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Contents 4 0 R/Annots[5 0 R]>> endobj\n\
4 0 obj <</Length 0>> stream\nendstream endobj\n\
5 0 obj <</Type/Annot/Subtype/Link/Rect[10 10 50 30]/Border[0 0 0]/H/N{action}>> endobj\n\
6 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Contents 7 0 R>> endobj\n\
7 0 obj <</Length {}>> stream\n{}endstream endobj\n\
trailer <</Root 1 0 R>>\n%%EOF",
                target_content.len(),
                target_content
            )
            .into_bytes(),
        )
    }

    fn catalog_navigation_pdf() -> Arc<[u8]> {
        Arc::from(
            &br#"%PDF-1.7
1 0 obj <</Type/Catalog/Pages 2 0 R/Names<</Dests 8 0 R>>/Dests 9 0 R/URI<</Base(https://glyphst.com/docs/guide.pdf)>>/Outlines 10 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R 6 0 R]/Count 2>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Contents 4 0 R>> endobj
4 0 obj <</Length 0>> stream
endstream endobj
5 0 obj <<>> endobj
6 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Contents 7 0 R>> endobj
7 0 obj <</Length 0>> stream
endstream endobj
8 0 obj <</Names[(chapter-two)[6 0 R/XYZ 12 34 2](fit-page)[3 0 R/Fit]]/Limits[(chapter-two)(fit-page)]>> endobj
9 0 obj <</legacy[6 0 R/FitH 40]>> endobj
10 0 obj <</Type/Outlines/First 11 0 R/Last 12 0 R/Count 3>> endobj
11 0 obj <</Title(Chapter One)/Parent 10 0 R/Next 12 0 R/First 13 0 R/Last 13 0 R/Count -1/Dest(fit-page)/C[0.1 0.2 0.3]/F 3>> endobj
12 0 obj <</Title(Chained)/Parent 10 0 R/Prev 11 0 R/A<</S/GoTo/D/legacy/Next[<</S/URI/URI(chapter.html#toc)>><</S/Named/N/LastPage>>]>>>> endobj
13 0 obj <</Title(Child)/Parent 11 0 R/Dest(chapter-two)>> endobj
trailer <</Root 1 0 R>>
%%EOF"#[..],
        )
    }

    fn cyclic_outline_pdf() -> Arc<[u8]> {
        Arc::from(
            &br#"%PDF-1.7
1 0 obj <</Type/Catalog/Pages 2 0 R/Outlines 5 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Contents 4 0 R>> endobj
4 0 obj <</Length 0>> stream
endstream endobj
5 0 obj <</Type/Outlines/First 6 0 R/Last 6 0 R/Count 1>> endobj
6 0 obj <</Title(Cycle)/Parent 5 0 R/Next 6 0 R>> endobj
trailer <</Root 1 0 R>>
%%EOF"#[..],
        )
    }

    fn multipage_pdf(page_count: usize) -> Arc<[u8]> {
        let kids = (0..page_count)
            .map(|index| format!("{} 0 R", index + 3))
            .collect::<Vec<_>>()
            .join(" ");
        let mut pdf = format!(
            "%PDF-1.4\n1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
             2 0 obj <</Type/Pages/Kids[{kids}]/Count {page_count}>> endobj\n"
        );
        for index in 0..page_count {
            pdf.push_str(&format!(
                "{} 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 200]>> endobj\n",
                index + 3
            ));
        }
        pdf.push_str("trailer <</Root 1 0 R>>\n%%EOF");
        Arc::from(pdf.into_bytes())
    }

    #[test]
    fn document_open_reuses_snapshot_bytes_and_compiles_owned_page() {
        let bytes = minimal_pdf();
        let snapshot =
            DocumentSnapshot::from_bytes("native.pdf", DocumentFormat::Pdf, Arc::clone(&bytes));
        assert!(Arc::ptr_eq(&bytes, &snapshot.shared_bytes()));

        let document = GpuPdfDocument::open(&snapshot).expect("open native document");
        assert_eq!(document.page_count(), 1);
        assert!(!document.is_encrypted());
        assert_eq!(document.permissions(), GpuPdfPermissions::default());
        let cancellation = CancellationToken::new();
        let first = document
            .compile_page(0, &cancellation)
            .expect("compile page");
        let second = document.compile_page(0, &cancellation).expect("reuse page");
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn document_navigation_retains_owned_outline_actions_hierarchy_and_style() {
        let snapshot = DocumentSnapshot::from_bytes(
            "catalog-navigation.pdf",
            DocumentFormat::Pdf,
            catalog_navigation_pdf(),
        );
        let document = GpuPdfDocument::open(&snapshot).expect("open native document");
        let GpuNavigationRoute::Ready(outline) = document.navigation_route() else {
            panic!("catalog navigation unexpectedly fell back");
        };

        assert_eq!(outline.len(), 2);
        assert_eq!(outline[0].title.as_ref(), "Chapter One");
        assert!(!outline[0].initially_open);
        assert_eq!(outline[0].color, Some([0.1, 0.2, 0.3]));
        assert!(outline[0].italic);
        assert!(outline[0].bold);
        assert_eq!(outline[0].children.len(), 1);
        assert_eq!(outline[0].children[0].title.as_ref(), "Child");
        assert_eq!(
            outline[0].action,
            Some(GpuLinkAction::GoTo {
                page: 0,
                view: GpuDestinationView::FitPage,
            })
        );
        assert_eq!(
            outline[0].children[0].action,
            Some(GpuLinkAction::GoTo {
                page: 1,
                view: GpuDestinationView::Xyz {
                    x: Some(12.0),
                    y: Some(66.0),
                    zoom: Some(2.0),
                },
            })
        );
        assert_eq!(
            outline[1].action,
            Some(GpuLinkAction::Sequence(Arc::from([
                GpuLinkAction::GoTo {
                    page: 1,
                    view: GpuDestinationView::FitWidth { top: Some(60.0) },
                },
                GpuLinkAction::Uri(Arc::from("https://glyphst.com/docs/chapter.html#toc")),
                GpuLinkAction::Named(GpuNamedAction::LastPage),
            ])))
        );
    }

    #[test]
    fn document_navigation_fallback_does_not_poison_native_page_routing() {
        let snapshot = DocumentSnapshot::from_bytes(
            "cyclic-outline.pdf",
            DocumentFormat::Pdf,
            cyclic_outline_pdf(),
        );
        let document = GpuPdfDocument::open(&snapshot).expect("open native document");
        let GpuNavigationRoute::Fallback(diagnostics) = document.navigation_route() else {
            panic!("cyclic outline unexpectedly passed navigation preflight");
        };
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.feature == FeatureId::Outlines
                && diagnostic.code == "outline-cycle-or-reuse"
                && diagnostic.source == GpuFallbackSource::Frontend
        }));
        assert!(
            document
                .frontend_diagnostics(0)
                .expect("preflight page")
                .is_empty()
        );
        assert!(
            document
                .compile_page(0, &CancellationToken::new())
                .expect("compile page")
                .support()
                .is_supported()
        );
    }

    #[test]
    fn frontend_preflight_preserves_exact_whole_page_fallback_reason() {
        let snapshot =
            DocumentSnapshot::from_bytes("overprint.pdf", DocumentFormat::Pdf, overprint_pdf());
        let document = GpuPdfDocument::open(&snapshot).expect("open native document");
        let diagnostics = document
            .frontend_diagnostics(0)
            .expect("preflight native page");

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.feature == FeatureId::Overprint
                && diagnostic.code == "overprint-not-simulated"
                && diagnostic.source == GpuFallbackSource::Frontend
        }));
    }

    #[test]
    fn application_link_mapping_failure_is_structured() {
        let diagnostic = GpuFallbackDiagnostic::application_link_geometry_mismatch();
        assert_eq!(diagnostic.feature, FeatureId::Links);
        assert_eq!(diagnostic.code, "application-link-geometry-mismatch");
        assert_eq!(diagnostic.source, GpuFallbackSource::Application);
    }

    #[test]
    fn compiled_link_interactions_retain_supported_modes_and_canonical_geometry() {
        for (highlight, expected_mode) in [
            ("", GpuLinkHighlightMode::Invert),
            ("/H/I", GpuLinkHighlightMode::Invert),
            ("/H/N", GpuLinkHighlightMode::None),
        ] {
            let snapshot =
                DocumentSnapshot::from_bytes("link.pdf", DocumentFormat::Pdf, link_pdf(highlight));
            let document = GpuPdfDocument::open(&snapshot).expect("open native document");
            let page = document
                .compile_page(0, &CancellationToken::new())
                .expect("compile page");
            let interactions = compiled_link_interactions(&page).expect("retain interactions");

            assert_eq!(
                interactions.as_ref(),
                &[GpuLinkInteraction {
                    annotation_bounds: [10.0, 70.0, 50.0, 90.0],
                    regions: Arc::from([[10.0, 70.0, 50.0, 70.0, 50.0, 90.0, 10.0, 90.0]]),
                    highlight_mode: expected_mode,
                    action: GpuLinkAction::Uri(Arc::from("https://example.com")),
                }]
            );
        }
    }

    #[test]
    fn malformed_null_link_destination_stays_native_as_an_inert_action() {
        let snapshot = DocumentSnapshot::from_bytes(
            "null-link-destination.pdf",
            DocumentFormat::Pdf,
            link_pdf_with_action("/Dest[null/XYZ null null null]"),
        );
        let document = GpuPdfDocument::open(&snapshot).expect("open native document");
        let page = document
            .compile_page(0, &CancellationToken::new())
            .expect("compile page");
        assert!(
            page.support().is_supported(),
            "{:#?}",
            page.support().diagnostics()
        );
        assert!(page.support().diagnostics().iter().any(|diagnostic| {
            diagnostic.code == "null-link-destination-noop"
                && diagnostic.severity == glyphst_pdf_engine::DiagnosticSeverity::Warning
        }));
        let interactions = compiled_link_interactions(&page).expect("retain inert interaction");
        assert!(matches!(
            &interactions[0].action,
            GpuLinkAction::NoOp { code, message }
                if code.as_ref() == "null-link-destination-noop" && message.contains("null")
        ));
        assert!(!interactions[0].action.requires_content_bounds());
        assert_eq!(
            document
                .resolve_link_action(&interactions[0].action, &CancellationToken::new())
                .expect("resolve inert action"),
            interactions[0].action
        );
    }

    #[test]
    fn compiled_link_interactions_own_partial_destinations_and_ordered_actions() {
        let snapshot = DocumentSnapshot::from_bytes(
            "partial-destination.pdf",
            DocumentFormat::Pdf,
            link_pdf_with_action("/Dest[6 0 R/XYZ 12 null 2]"),
        );
        let document = GpuPdfDocument::open(&snapshot).expect("open native document");
        let page = document
            .compile_page(0, &CancellationToken::new())
            .expect("compile page");
        assert!(
            page.support().is_supported(),
            "unexpected diagnostics: {:#?}",
            page.support().diagnostics()
        );
        let interactions = compiled_link_interactions(&page).expect("retain interactions");
        assert_eq!(
            interactions[0].action,
            GpuLinkAction::GoTo {
                page: 1,
                view: GpuDestinationView::Xyz {
                    x: Some(12.0),
                    y: None,
                    zoom: Some(2.0),
                },
            }
        );

        let snapshot = DocumentSnapshot::from_bytes(
            "action-chain.pdf",
            DocumentFormat::Pdf,
            link_pdf_with_action(
                "/A<</S/Named/N/FirstPage/Next[<</S/Named/N/NextPage>><</S/Named/N/LastPage>>]>>",
            ),
        );
        let document = GpuPdfDocument::open(&snapshot).expect("open native document");
        let page = document
            .compile_page(0, &CancellationToken::new())
            .expect("compile page");
        assert!(
            page.support().is_supported(),
            "unexpected diagnostics: {:#?}",
            page.support().diagnostics()
        );
        let interactions = compiled_link_interactions(&page).expect("retain interactions");
        assert_eq!(
            interactions[0].action,
            GpuLinkAction::Sequence(Arc::from([
                GpuLinkAction::Named(GpuNamedAction::FirstPage),
                GpuLinkAction::Named(GpuNamedAction::NextPage),
                GpuLinkAction::Named(GpuNamedAction::LastPage),
            ]))
        );

        let snapshot = DocumentSnapshot::from_bytes(
            "relative-uri.pdf",
            DocumentFormat::Pdf,
            link_pdf_with_catalog_and_action(
                "/URI<</Base(https://glyphst.com/docs/guide.pdf)>>",
                "/A<</S/URI/URI(../chapter.html?mode=read#intro)>>",
            ),
        );
        let document = GpuPdfDocument::open(&snapshot).expect("open native document");
        let page = document
            .compile_page(0, &CancellationToken::new())
            .expect("compile page");
        assert!(
            page.support().is_supported(),
            "unexpected diagnostics: {:#?}",
            page.support().diagnostics()
        );
        let interactions = compiled_link_interactions(&page).expect("retain interactions");
        assert_eq!(
            interactions[0].action,
            GpuLinkAction::Uri(Arc::from(
                "https://glyphst.com/chapter.html?mode=read#intro"
            ))
        );

        let snapshot = DocumentSnapshot::from_bytes(
            "uri-map.pdf",
            DocumentFormat::Pdf,
            link_pdf_with_catalog_and_action(
                "/URI<</Base(https://glyphst.com/docs/guide.pdf)>>",
                "/A<</S/URI/URI(../map)/IsMap true>>",
            ),
        );
        let document = GpuPdfDocument::open(&snapshot).expect("open native document");
        let page = document
            .compile_page(0, &CancellationToken::new())
            .expect("compile page");
        let interactions = compiled_link_interactions(&page).expect("retain interactions");
        let GpuLinkAction::UriMap(action) = &interactions[0].action else {
            panic!("relative URI IsMap action was not retained")
        };
        assert_eq!(action.uri(), "https://glyphst.com/map");
        assert_eq!(
            action.resolve_at(30.0, 80.0).as_deref(),
            Some("https://glyphst.com/map?20,10")
        );
        assert!(action.resolve_at(f64::NAN, 80.0).is_none());
    }

    #[test]
    fn content_bounding_box_destinations_resolve_lazily_and_cache_one_target() {
        let snapshot = DocumentSnapshot::from_bytes(
            "fit-bounding-box.pdf",
            DocumentFormat::Pdf,
            link_pdf_with_action(
                "/A<</S/GoTo/D[6 0 R/FitB]/Next[<</S/GoTo/D[6 0 R/FitBH null]>><</S/GoTo/D[6 0 R/FitBV 12]>>]>>",
            ),
        );
        let document = GpuPdfDocument::open(&snapshot).expect("open native document");
        let page = document
            .compile_page(0, &CancellationToken::new())
            .expect("compile page");
        assert!(
            page.support().is_supported(),
            "unexpected diagnostics: {:#?}",
            page.support().diagnostics()
        );
        let interactions = compiled_link_interactions(&page).expect("retain lazy FitB actions");
        assert!(interactions[0].action.requires_content_bounds());
        assert_eq!(document.content_bounds_cache_entries(), 0);
        let resolved = document
            .resolve_link_action(&interactions[0].action, &CancellationToken::new())
            .expect("resolve FitB action tree");
        assert!(!resolved.requires_content_bounds());
        assert_eq!(document.content_bounds_cache_entries(), 1);
        assert_eq!(
            resolved,
            GpuLinkAction::Sequence(Arc::from([
                GpuLinkAction::GoTo {
                    page: 1,
                    view: GpuDestinationView::FitBoundingBox {
                        content_bounds: Some([20.0, 20.0, 60.0, 70.0]),
                    },
                },
                GpuLinkAction::GoTo {
                    page: 1,
                    view: GpuDestinationView::FitBoundingBoxWidth {
                        top: None,
                        content_bounds: Some([20.0, 20.0, 60.0, 70.0]),
                    },
                },
                GpuLinkAction::GoTo {
                    page: 1,
                    view: GpuDestinationView::FitBoundingBoxHeight {
                        left: Some(12.0),
                        content_bounds: Some([20.0, 20.0, 60.0, 70.0]),
                    },
                },
            ]))
        );

        let cancelled_snapshot = DocumentSnapshot::from_bytes(
            "cancelled-fit-bounding-box.pdf",
            DocumentFormat::Pdf,
            link_pdf_with_action("/Dest[6 0 R/FitB]"),
        );
        let cancelled_document =
            GpuPdfDocument::open(&cancelled_snapshot).expect("open cancellable document");
        let page = cancelled_document
            .compile_page(0, &CancellationToken::new())
            .expect("compile cancellable source page");
        let interactions = compiled_link_interactions(&page).expect("retain cancellable action");
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert_eq!(
            cancelled_document.resolve_link_action(&interactions[0].action, &cancellation),
            Err(GpuActionResolutionError::Cancelled)
        );
        assert_eq!(cancelled_document.content_bounds_cache_entries(), 0);

        let snapshot = DocumentSnapshot::from_bytes(
            "large-destination.pdf",
            DocumentFormat::Pdf,
            link_pdf_with_action(
                "/Dest[6 0 R/XYZ 9999999999999999999999999999999999999999 null null]",
            ),
        );
        let document = GpuPdfDocument::open(&snapshot).expect("open native document");
        let page = document
            .compile_page(0, &CancellationToken::new())
            .expect("compile page");
        assert!(
            page.support().is_supported(),
            "unexpected diagnostics: {:#?}",
            page.support().diagnostics()
        );
        let diagnostic = compiled_link_interactions(&page)
            .expect_err("the f32 reader boundary must reject an oversized coordinate");
        assert_eq!(
            diagnostic.code,
            "application-destination-coordinate-out-of-range"
        );
    }

    #[test]
    fn compiled_link_interactions_hit_test_exact_multi_quads() {
        let snapshot = DocumentSnapshot::from_bytes(
            "link-quads.pdf",
            DocumentFormat::Pdf,
            link_pdf_with_geometry(
                "/H/N",
                "[10 10 90 50]",
                "/QuadPoints[10 10 40 10 38 20 12 20 50 30 90 30 88 50 52 50]",
            ),
        );
        let document = GpuPdfDocument::open(&snapshot).expect("open native document");
        let page = document
            .compile_page(0, &CancellationToken::new())
            .expect("compile page");
        let interactions = compiled_link_interactions(&page).expect("retain interactions");
        let interaction = &interactions[0];

        assert_eq!(interaction.annotation_bounds, [10.0, 50.0, 90.0, 90.0]);
        assert_eq!(interaction.regions.len(), 2);
        assert!(interaction.contains_point(20.0, 85.0));
        assert!(interaction.contains_point(70.0, 60.0));
        assert!(interaction.contains_point(12.0, 80.0));
        assert!(!interaction.contains_point(10.5, 80.5));
        assert!(!interaction.contains_point(45.0, 75.0));
        assert!(!interaction.contains_point(f64::NAN, 85.0));
    }

    #[test]
    fn application_boundary_types_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<GpuPdfDocument>();
        assert_send_sync::<PreparedGpuPage>();
        assert_send_sync::<GpuPdfRenderer>();
        assert_send_sync::<GpuPageTexture>();
        assert_send_sync::<GpuPageRegion>();
        assert_send_sync::<GpuLinkInteraction>();
        assert_send_sync::<GpuOutlineItem>();
        assert_send_sync::<GpuNavigationRoute>();
    }

    #[test]
    fn compiled_scene_cache_evicts_least_recently_used_pages() {
        let snapshot = DocumentSnapshot::from_bytes(
            "many-pages.pdf",
            DocumentFormat::Pdf,
            multipage_pdf(MAX_COMPILED_PAGE_CACHE_ENTRIES + 1),
        );
        let document = GpuPdfDocument::open(&snapshot).expect("open native document");
        let cancellation = CancellationToken::new();
        let first = document
            .compile_page(0, &cancellation)
            .expect("compile first page");
        for page in 1..=MAX_COMPILED_PAGE_CACHE_ENTRIES {
            document
                .compile_page(page as u32, &cancellation)
                .expect("compile subsequent page");
        }

        assert_eq!(
            document.compiled_cache_entries(),
            MAX_COMPILED_PAGE_CACHE_ENTRIES
        );
        let recompiled = document
            .compile_page(0, &cancellation)
            .expect("recompile evicted page");
        assert!(!Arc::ptr_eq(&first, &recompiled));
    }

    #[test]
    fn content_bounds_cache_is_bounded_and_evicts_least_recently_used_pages() {
        let mut cache = ContentBoundsCache::default();
        for page in 0..=MAX_CONTENT_BOUNDS_CACHE_ENTRIES {
            cache
                .insert(
                    PageIndex::new(page as u32),
                    Ok([page as f64, 0.0, page as f64 + 1.0, 1.0]),
                )
                .expect("cache content bounds");
        }
        assert_eq!(cache.entries.len(), MAX_CONTENT_BOUNDS_CACHE_ENTRIES);
        assert!(cache.get(PageIndex::new(0)).is_none());
        assert_eq!(
            cache.get(PageIndex::new(MAX_CONTENT_BOUNDS_CACHE_ENTRIES as u32)),
            Some(Ok([
                MAX_CONTENT_BOUNDS_CACHE_ENTRIES as f64,
                0.0,
                MAX_CONTENT_BOUNDS_CACHE_ENTRIES as f64 + 1.0,
                1.0,
            ]))
        );
    }

    #[test]
    fn quarter_turns_map_page_bounds_to_the_render_target() {
        let bounds = glyphst_pdf_engine::Rect::from_corners(
            glyphst_pdf_engine::Point::new(10.0, 20.0).unwrap(),
            glyphst_pdf_engine::Point::new(110.0, 220.0).unwrap(),
        )
        .unwrap();
        for rotation in [
            DocumentRotation::Degrees0,
            DocumentRotation::Degrees90,
            DocumentRotation::Degrees180,
            DocumentRotation::Degrees270,
        ] {
            let target = if rotation.swaps_width_and_height() {
                [600, 300]
            } else {
                [150, 300]
            };
            let transform = page_transform(bounds, rotation, target).unwrap();
            let mapped = transform.map_rect(bounds);
            assert!((mapped.x0 - 0.0).abs() < 1e-8);
            assert!((mapped.y0 - 0.0).abs() < 1e-8);
            assert!((mapped.x1 - f64::from(target[0])).abs() < 1e-8);
            assert!((mapped.y1 - f64::from(target[1])).abs() < 1e-8);
        }
    }

    #[test]
    fn independently_rounded_reader_sizes_keep_a_uniform_page_root() {
        let bounds = glyphst_pdf_engine::Rect::from_corners(
            glyphst_pdf_engine::Point::new(0.0, 0.0).unwrap(),
            glyphst_pdf_engine::Point::new(595.275_573_730_468_8, 841.889_770_507_812_5).unwrap(),
        )
        .unwrap();
        let nominal_scale = 1.337_f64;
        for rotation in [
            DocumentRotation::Degrees0,
            DocumentRotation::Degrees90,
            DocumentRotation::Degrees180,
            DocumentRotation::Degrees270,
        ] {
            let (width, height) = if rotation.swaps_width_and_height() {
                (bounds.height(), bounds.width())
            } else {
                (bounds.width(), bounds.height())
            };
            let target = [
                (width * nominal_scale).ceil() as u32,
                (height * nominal_scale).ceil() as u32,
            ];
            let transform = page_transform(bounds, rotation, target).unwrap();
            let [a, b, c, d, _, _] = transform.coefficients();
            let first = a * a + b * b;
            let second = c * c + d * d;
            assert!((first - second).abs() <= first.max(second) * 1.0e-12);
            assert!((a * c + b * d).abs() <= first.max(second) * 1.0e-12);
            assert!(a * d - b * c > 0.0);

            let mapped = transform.map_rect(bounds);
            assert!(mapped.x0 >= -1.0e-8 && mapped.y0 >= -1.0e-8);
            assert!(mapped.x1 <= f64::from(target[0]) + 1.0e-8);
            assert!(mapped.y1 <= f64::from(target[1]) + 1.0e-8);
            assert!((mapped.x0 - (f64::from(target[0]) - mapped.x1)).abs() < 1.0e-8);
            assert!((mapped.y0 - (f64::from(target[1]) - mapped.y1)).abs() < 1.0e-8);
        }
    }

    #[test]
    fn visible_regions_are_clamped_and_expanded_by_one_analytic_tile() {
        let request = GpuPaintRequest {
            page_size: [1_000, 2_000],
            visible_region: GpuPageRegion {
                x: 200,
                y: 500,
                width: 300,
                height: 400,
            },
            rotation: DocumentRotation::Degrees0,
            background: [1.0; 4],
            settled: false,
            generation: 1,
        };
        assert_eq!(
            request.surface_region().unwrap(),
            GpuPageRegion {
                x: 72,
                y: 372,
                width: 556,
                height: 656,
            }
        );

        let edge = GpuPaintRequest {
            visible_region: GpuPageRegion {
                x: 950,
                y: 1_950,
                width: 100,
                height: 100,
            },
            ..request
        };
        assert_eq!(
            edge.surface_region().unwrap(),
            GpuPageRegion {
                x: 822,
                y: 1_822,
                width: 178,
                height: 178,
            }
        );
    }
}
