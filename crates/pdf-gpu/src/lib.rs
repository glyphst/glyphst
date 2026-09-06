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
    OutlineItem as EngineOutlineItem, PageIndex, Password, Rect, ResourceLimits, SupportDiagnostic,
    Transform,
};
pub use glyphst_pdf_engine::{
    COMPILED_PAGE_CACHE_VERSION, CancellationToken, DocumentCacheStats as GpuDocumentCacheStats,
    FeatureId, Permissions as GpuPdfPermissions,
};
use glyphst_pdf_render_cpu::{CpuRenderer, CpuViewportRequest};
use glyphst_pdf_render_wgpu::{
    ANALYTIC_TILE_SIZE, DeviceToken, GpuPage, GpuRenderer, IncrementalRenderState,
    IncrementalWorkPlan, PrepareError, RenderConfig, RenderError, RenderRequest,
    RenderSupportIssue, RenderTargetExtentPolicy, RendererInitError, RetainedStrokeStrategy,
    output_format_supported,
};
pub use glyphst_pdf_render_wgpu::{
    IncrementalWorkKind as GpuIncrementalWorkKind, IncrementalWorkPlan as GpuIncrementalWorkPlan,
    MotionRenderMode as GpuMotionRenderMode,
    PatchLodPreparationStats as GpuPatchLodPreparationStats, PatchLodStatus as GpuPatchLodStatus,
    PreparationStats as GpuPreparationStats, PresentationQuality as GpuPresentationQuality,
    RenderStats as GpuRenderStats, VectorCoverageStrategy as GpuVectorCoverageStrategy,
    VectorLodStatus as GpuVectorLodStatus,
};
use gpui::{
    ExternalGpuContext, ExternalGpuDeviceToken, ExternalGpuRenderHandle, ExternalTextureHandle,
    ExternalTextureWriteReservation,
};
use gpui_wgpu::{WgpuExternalContext, WgpuExternalRender, WgpuExternalRenderContext, wgpu};
use std::{
    collections::HashMap,
    fmt, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant, UNIX_EPOCH},
};

const MAX_COMPILED_PAGE_CACHE_ENTRIES: usize = 8;
const MAX_CONTENT_BOUNDS_CACHE_ENTRIES: usize = 128;
const MAX_DISK_COMPILED_CACHE_ENTRIES: usize = 64;
const MAX_DISK_COMPILED_CACHE_BYTES: u64 = 1024 * 1024 * 1024;
const COMPILED_CACHE_IDLE_DELAY: Duration = Duration::from_secs(5);
// ponytail: one best-effort writer avoids cache-induced stalls; add an idle
// queue only if multi-page cache coverage proves more valuable than latency.
static COMPILED_CACHE_WRITER_ACTIVE: AtomicBool = AtomicBool::new(false);
const MAX_PRESENTATION_SURFACES: usize = 7;
const CAMERA_TARGET_COUNT: usize = 2;
const PAN_CACHE_TARGET_COUNT: usize = 3;
const PAN_CACHE_GUTTER_MIN: u32 = 96;

fn pan_cache_gutter(extent: u32, overscan_axis: bool) -> u32 {
    // Glyphst's measured strict-render cache is 1.7x the viewport with a 96 px
    // minimum border on each side. The reader may keep only that minimum on
    // the cross axis of a one-dimensional gesture; integer arithmetic keeps
    // both contracts deterministic before tile alignment.
    if overscan_axis {
        extent
            .saturating_mul(7)
            .div_ceil(20)
            .max(PAN_CACHE_GUTTER_MIN)
    } else {
        PAN_CACHE_GUTTER_MIN
    }
}

fn prune_compiled_page_cache(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut entries = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            (metadata.is_file() && entry.path().extension().is_some_and(|ext| ext == "zst")).then(
                || {
                    (
                        metadata.modified().unwrap_or(UNIX_EPOCH),
                        metadata.len(),
                        entry.path(),
                    )
                },
            )
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.0);
    let mut remaining = entries.len();
    let mut bytes = entries
        .iter()
        .map(|entry| entry.1)
        .fold(0_u64, u64::saturating_add);
    for (_, len, path) in entries {
        if remaining <= MAX_DISK_COMPILED_CACHE_ENTRIES && bytes <= MAX_DISK_COMPILED_CACHE_BYTES {
            break;
        }
        if fs::remove_file(path).is_ok() {
            remaining = remaining.saturating_sub(1);
            bytes = bytes.saturating_sub(len);
        }
    }
}

#[cfg(unix)]
fn restrict_private_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_private_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_private_file(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_private_file(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Parser/interpreter or WGPU capability that requires whole-page fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuFallbackDiagnostic {
    pub feature: FeatureId,
    pub code: String,
    pub message: String,
    pub source: GpuFallbackSource,
}

/// Observable frontend recovery that does not require whole-page fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuWarningDiagnostic {
    pub feature: FeatureId,
    pub code: String,
    pub message: String,
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
    /// Whether this action chain has no executable effect.
    #[must_use]
    pub fn is_inert(&self) -> bool {
        match self {
            Self::NoOp { .. } => true,
            Self::Sequence(actions) => actions.iter().all(Self::is_inert),
            Self::GoTo { .. } | Self::Uri(_) | Self::UriMap(_) | Self::Named(_) => false,
        }
    }

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

impl From<&SupportDiagnostic> for GpuWarningDiagnostic {
    fn from(diagnostic: &SupportDiagnostic) -> Self {
        Self {
            feature: diagnostic.feature,
            code: diagnostic.code.clone(),
            message: diagnostic.message.clone(),
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
    limits: ResourceLimits,
    navigation: GpuNavigationRoute,
    compiled_cache_root: Option<PathBuf>,
    cache_activity: AtomicU64,
    compiled: Mutex<CompiledPageCache>,
    content_bounds: Mutex<ContentBoundsCache>,
}

struct CompiledPageCache {
    entries: HashMap<PageIndex, CachedCompiledPage>,
    clock: u64,
    bytes: u64,
    byte_limit: u64,
}

struct CachedCompiledPage {
    page: Arc<CompiledPage>,
    last_used: u64,
    bytes: u64,
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
    fn new(byte_limit: u64) -> Self {
        Self {
            entries: HashMap::new(),
            clock: 0,
            bytes: 0,
            byte_limit,
        }
    }

    fn get(&mut self, index: PageIndex) -> Option<Arc<CompiledPage>> {
        self.clock = self.clock.wrapping_add(1).max(1);
        let entry = self.entries.get_mut(&index)?;
        entry.last_used = self.clock;
        Some(Arc::clone(&entry.page))
    }

    fn insert(
        &mut self,
        index: PageIndex,
        page: Arc<CompiledPage>,
        bytes: u64,
    ) -> Arc<CompiledPage> {
        self.clock = self.clock.wrapping_add(1).max(1);
        if let Some(entry) = self.entries.get_mut(&index) {
            entry.last_used = self.clock;
            return Arc::clone(&entry.page);
        }
        // An oversized page can still be rendered; it must not flush useful cache entries.
        if bytes > self.byte_limit {
            return page;
        }
        while self.entries.len() >= MAX_COMPILED_PAGE_CACHE_ENTRIES
            || self.bytes > self.byte_limit - bytes
        {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(index, _)| *index)
            else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.bytes -= entry.bytes;
            }
        }
        self.entries.insert(
            index,
            CachedCompiledPage {
                page: Arc::clone(&page),
                last_used: self.clock,
                bytes,
            },
        );
        self.bytes += bytes;
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
        Self::open_inner(snapshot, None)
    }

    /// Open with a bounded persistent cache for validated compiled pages.
    pub fn open_with_compiled_cache(
        snapshot: &DocumentSnapshot,
        cache_root: impl Into<PathBuf>,
    ) -> Result<Self> {
        Self::open_inner(snapshot, Some(cache_root.into()))
    }

    fn open_inner(snapshot: &DocumentSnapshot, cache_root: Option<PathBuf>) -> Result<Self> {
        let options = OpenOptions {
            password: snapshot
                .password()
                .map(|password| Password::new(password.expose_secret())),
            ..OpenOptions::default()
        };
        let limits = options.limits.clone();
        let document = Engine::open(DocumentSource::from_bytes(snapshot.shared_bytes()), options)
            .context("failed to open PDF with the native GPU engine")?;
        let navigation = compiled_document_navigation(&document);
        let compiled_cache_root = (!document.is_encrypted()).then_some(cache_root).flatten();
        let compiled = Mutex::new(CompiledPageCache::new(limits.max_cpu_cache_bytes));
        Ok(Self {
            inner: Arc::new(GpuPdfDocumentInner {
                document,
                limits,
                navigation,
                compiled_cache_root,
                cache_activity: AtomicU64::new(0),
                compiled,
                content_bounds: Mutex::new(ContentBoundsCache::default()),
            }),
        })
    }

    /// Number of checked pages in the native engine.
    pub fn page_count(&self) -> u32 {
        self.inner.document.page_count().get()
    }

    /// Defers best-effort cache serialization until reader interaction is idle.
    pub fn note_interaction(&self) {
        self.inner.cache_activity.fetch_add(1, Ordering::Release);
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

    /// Current document-owned parser/interpreter cache statistics.
    #[must_use]
    pub fn frontend_cache_stats(&self) -> GpuDocumentCacheStats {
        self.inner.document.cache_stats()
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

    /// Estimated bytes owned by cached compiled pages, excluding other live page references.
    pub fn compiled_cache_bytes(&self) -> u64 {
        self.inner
            .compiled
            .lock()
            .map(|cache| cache.bytes)
            .unwrap_or_default()
    }

    pub fn compiled_cache_byte_limit(&self) -> u64 {
        self.inner.limits.max_cpu_cache_bytes
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
        self.compile_page_with_cache_status(page, cancellation)
            .map(|(page, _)| page)
    }

    fn compile_page_with_cache_status(
        &self,
        page: u32,
        cancellation: &CancellationToken,
    ) -> Result<(Arc<CompiledPage>, bool)> {
        let page = self.checked_page(page)?;
        if let Some(compiled) = self
            .inner
            .compiled
            .lock()
            .map_err(|_| anyhow!("native PDF page cache is poisoned"))?
            .get(page)
        {
            return Ok((compiled, true));
        }

        let options = CompileOptions::default();
        if !cancellation.is_cancelled()
            && let Some(compiled) = self.load_compiled_page_cache(page, &options)
        {
            let bytes = compiled.estimated_retained_bytes();
            let compiled = self
                .inner
                .compiled
                .lock()
                .map_err(|_| anyhow!("native PDF page cache is poisoned"))?
                .insert(page, compiled, bytes);
            return Ok((compiled, true));
        }

        let compiled = self
            .inner
            .document
            .compile_page(page, options, cancellation)
            .with_context(|| format!("failed to compile native PDF page {}", page.get() + 1))?;
        let bytes = compiled.estimated_retained_bytes();
        let compiled = self
            .inner
            .compiled
            .lock()
            .map_err(|_| anyhow!("native PDF page cache is poisoned"))?
            .insert(page, compiled, bytes);
        Ok((compiled, false))
    }

    fn compiled_page_cache_path(&self, page: PageIndex) -> Option<PathBuf> {
        let root = self.inner.compiled_cache_root.as_ref()?;
        let source = self.inner.document.source_fingerprint().to_hex();
        Some(root.join(format!("{source}-page-{}.cbor.zst", page.get())))
    }

    fn load_compiled_page_cache(
        &self,
        page: PageIndex,
        options: &CompileOptions,
    ) -> Option<Arc<CompiledPage>> {
        let path = self.compiled_page_cache_path(page)?;
        let limit = self.inner.limits.max_cpu_cache_bytes;
        let metadata = fs::metadata(&path).ok()?;
        if metadata.len() > limit {
            let _ignored = fs::remove_file(path);
            return None;
        }
        let file = fs::File::open(&path).ok()?;
        let Ok(decoder) = zstd::stream::read::Decoder::new(file) else {
            let _ignored = fs::remove_file(path);
            return None;
        };
        let mut archive = Vec::new();
        if decoder
            .take(limit.saturating_add(1))
            .read_to_end(&mut archive)
            .is_err()
        {
            let _ignored = fs::remove_file(path);
            return None;
        }
        if archive.len() as u64 > limit {
            let _ignored = fs::remove_file(path);
            return None;
        }
        match self
            .inner
            .document
            .decode_compiled_page_cache(page, options, &archive)
        {
            Ok(compiled) => Some(compiled),
            Err(_) => {
                let _ignored = fs::remove_file(path);
                None
            }
        }
    }

    fn persist_compiled_page_cache(&self, page: &CompiledPage) -> Result<()> {
        let Some(path) = self.compiled_page_cache_path(page.index()) else {
            return Ok(());
        };
        let archive = self
            .inner
            .document
            .encode_compiled_page_cache(page, &CompileOptions::default())
            .context("failed to encode native PDF compiled-page cache")?;
        if archive.len() as u64 > self.inner.limits.max_cpu_cache_bytes {
            return Ok(());
        }
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("native PDF compiled-page cache has no parent"))?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create native PDF cache {}", parent.display()))?;
        restrict_private_directory(parent)?;
        let compressed = zstd::stream::encode_all(archive.as_slice(), 3)
            .context("failed to compress native PDF compiled-page cache")?;
        let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
        let mut file = fs::File::create(&temporary).with_context(|| {
            format!("failed to create native PDF cache {}", temporary.display())
        })?;
        file.write_all(&compressed)
            .with_context(|| format!("failed to write native PDF cache {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to flush native PDF cache {}", temporary.display()))?;
        restrict_private_file(&temporary)?;
        fs::rename(&temporary, &path)
            .with_context(|| format!("failed to publish native PDF cache {}", path.display()))?;
        prune_compiled_page_cache(parent);
        Ok(())
    }

    fn schedule_compiled_page_cache_write(&self, page: Arc<CompiledPage>) {
        if self.inner.compiled_cache_root.is_none()
            || COMPILED_CACHE_WRITER_ACTIVE
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        let document = self.clone();
        let mut activity = self.inner.cache_activity.load(Ordering::Acquire);
        if std::thread::Builder::new()
            .name("glyphst-pdf-cache".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(COMPILED_CACHE_IDLE_DELAY);
                    let current = document.inner.cache_activity.load(Ordering::Acquire);
                    if current == activity {
                        break;
                    }
                    activity = current;
                }
                if let Err(error) = document.persist_compiled_page_cache(&page) {
                    eprintln!("failed to persist native PDF compiled-page cache: {error:#}");
                }
                COMPILED_CACHE_WRITER_ACTIVE.store(false, Ordering::Release);
            })
            .is_err()
        {
            COMPILED_CACHE_WRITER_ACTIVE.store(false, Ordering::Release);
        }
    }

    /// Deterministically renders a complete page through the native CPU
    /// backend for export. This never reads GPU presentation pixels back.
    pub fn render_page_image(
        &self,
        page: u32,
        rotation: DocumentRotation,
        pixels_per_point: f32,
        cancellation: &CancellationToken,
    ) -> Result<NativePageImage> {
        if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
            bail!("native PDF export scale must be finite and positive");
        }
        let compiled = self.compile_page(page, cancellation)?;
        if let Some(diagnostic) = compiled
            .support()
            .diagnostics()
            .iter()
            .find(|diagnostic| diagnostic.severity == DiagnosticSeverity::Unsupported)
        {
            bail!(
                "native PDF export is unsupported [{}:{}]: {}",
                diagnostic.feature,
                diagnostic.code,
                diagnostic.message
            );
        }
        let bounds = compiled.scene().page_bounds();
        let (width, height) = if rotation.swaps_width_and_height() {
            (bounds.height(), bounds.width())
        } else {
            (bounds.width(), bounds.height())
        };
        let output_dimension = |points: f64| -> Result<u32> {
            let pixels = points * f64::from(pixels_per_point);
            if !pixels.is_finite() || pixels <= 0.0 || pixels > f64::from(u32::MAX) {
                bail!("native PDF export dimensions are invalid");
            }
            Ok(pixels.ceil() as u32)
        };
        let pixel_size = [output_dimension(width)?, output_dimension(height)?];
        let page_to_device = page_transform(bounds, rotation, pixel_size)?;
        let image = CpuRenderer::new(self.inner.limits.clone())
            .render_viewport(
                compiled.scene(),
                CpuViewportRequest {
                    page_to_device,
                    pixel_size,
                    background: [255; 4],
                },
                cancellation,
            )
            .context("failed to execute native PDF CPU export")?;
        Ok(NativePageImage {
            width: image.width,
            height: image.height,
            rgba: image.rgba,
        })
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

/// Straight sRGBA8 page export produced by the native CPU backend.
#[derive(Clone, Debug)]
pub struct NativePageImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
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
    warnings: Arc<[GpuWarningDiagnostic]>,
    timings: GpuPagePreparationTimings,
}

/// Reader-relevant cold stages for one prepared page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuPagePreparationTimings {
    pub frontend_preflight: Duration,
    pub compile: Duration,
    pub link_conversion: Duration,
    pub gpu_prepare: Duration,
    pub total: Duration,
    pub compiled_cache_hit: bool,
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

    /// Interactive presentation path selected by the retained renderer.
    pub fn motion_render_mode(&self) -> GpuMotionRenderMode {
        self.gpu.motion_render_mode()
    }

    pub const fn preparation_timings(&self) -> GpuPagePreparationTimings {
        self.timings
    }

    pub fn patch_lod_preparation_stats(&self) -> GpuPatchLodPreparationStats {
        self.gpu.patch_lod_preparation_stats()
    }

    /// Exact Link press regions that must be honored by the presentation UI.
    pub fn link_interactions(&self) -> &[GpuLinkInteraction] {
        &self.link_interactions
    }

    /// Deterministic frontend repairs retained for diagnostics and inspection.
    pub fn warnings(&self) -> &[GpuWarningDiagnostic] {
        &self.warnings
    }
}

fn compiled_page_warnings(page: &CompiledPage) -> Arc<[GpuWarningDiagnostic]> {
    page.support()
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Warning)
        .map(GpuWarningDiagnostic::from)
        .collect::<Vec<_>>()
        .into()
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
            let mut interaction = GpuLinkInteraction {
                annotation_bounds,
                regions: regions.into(),
                highlight_mode,
                action,
            };
            if !link.visible {
                // Keep the bounds and entry order for the MuPDF link mapping.
                // A layer-hidden link must never hit, highlight, or activate.
                interaction.regions = Arc::from([]);
                interaction.highlight_mode = GpuLinkHighlightMode::None;
                interaction.action = GpuLinkAction::NoOp {
                    code: Arc::from("annotation-layer-hidden"),
                    message: Arc::from("the link's annotation layer is hidden"),
                };
            }
            Ok(interaction)
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

/// Result of one queued native-page render attempt.
#[derive(Debug)]
// Keep successful frame metrics inline: boxing this hot-path result would add
// one allocator round trip to every interactive GPU submission.
#[allow(clippy::large_enum_variant)]
pub enum GpuPageRenderOutcome {
    /// The newest request submitted successfully and advanced working state.
    /// `presented` is absent while a bounded settled frame remains incomplete.
    Rendered {
        presented: Option<GpuPageTexture>,
        stats: GpuRenderStats,
    },
    /// Planning proved this request needs a full redraw that the caller asked
    /// to postpone until interaction settles. No target texture was allocated.
    Deferred,
    /// A newer request invalidated this attempt before queue submission.
    Superseded,
}

/// Policy for one queued native-page render attempt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuPageRenderOptions {
    /// Return [`GpuPageRenderOutcome::Deferred`] when incremental planning
    /// selects a full redraw. Direct redraws, reprojection, and refinement are
    /// still allowed so retained presentation work remains responsive.
    pub defer_full_render: bool,
}

/// Same-device PDF renderer tied to one GPUI recovery generation.
pub struct GpuPdfRenderer {
    context: WgpuExternalContext,
    renderer: Arc<GpuRenderer>,
    output_format: wgpu::TextureFormat,
    motion_submitted: Arc<AtomicU64>,
    motion_completed: Arc<AtomicU64>,
    direct_render: DirectRenderTelemetry,
}

#[derive(Default)]
struct DirectRenderTelemetry {
    scheduled: AtomicU64,
    executed: AtomicU64,
    encoded: AtomicU64,
    superseded: AtomicU64,
    failed: AtomicU64,
    last_scheduled_camera_serial: AtomicU64,
    last_executed_camera_serial: AtomicU64,
    last_encoded_camera_serial: AtomicU64,
    latest_stats: Mutex<Option<([u32; 2], GpuRenderStats)>>,
}

/// Execution counters for painter-ordered PDF callbacks in GPUI frames.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GpuDirectRenderStats {
    pub scheduled: u64,
    pub executed: u64,
    pub encoded: u64,
    pub superseded: u64,
    pub failed: u64,
    pub last_scheduled_camera_serial: u64,
    pub last_executed_camera_serial: u64,
    pub last_encoded_camera_serial: u64,
    pub last_cpu_time: Duration,
    pub last_frame_pool_wait_time: Duration,
    pub last_command_encoding_time: Duration,
    pub last_frame_pool_grew: bool,
    pub last_frame_pool_capacity_bytes: u64,
    pub last_target_size: [u32; 2],
    pub last_passes_executed: u64,
    pub last_coverage_strategy: Option<GpuVectorCoverageStrategy>,
    pub last_presentation_quality: Option<GpuPresentationQuality>,
    pub last_transient_peak_bytes: u64,
    pub last_transient_resident_bytes: u64,
    pub last_render_bundle_runs: u64,
    pub last_stencil_draws: u64,
    pub last_retained_stroke_candidate_references: u64,
    pub last_retained_stroke_cull_dispatches: u64,
    pub last_retained_stroke_indirect_draws: u64,
    pub last_retained_stroke_indirect_runs: u64,
    pub last_selected_stroke_lod: u32,
    pub last_active_stroke_lod_levels: u64,
    pub last_selected_stroke_segments: u64,
    pub last_target_visible_stroke_segments: u64,
    pub last_visible_stroke_tiles: u64,
    pub last_max_visible_stroke_tile_segments: u64,
}

/// Final disposition of one painter-ordered direct PDF draw.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GpuDirectRenderOutcome {
    #[default]
    Pending,
    Encoded,
    Superseded,
    Failed,
}

impl GpuDirectRenderOutcome {
    const fn as_u8(self) -> u8 {
        match self {
            Self::Pending => 0,
            Self::Encoded => 1,
            Self::Superseded => 2,
            Self::Failed => 3,
        }
    }

    const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Encoded,
            2 => Self::Superseded,
            3 => Self::Failed,
            _ => Self::Pending,
        }
    }
}

struct DirectRenderReceiptState {
    camera_serial: u64,
    generation: u64,
    outcome: AtomicU8,
    snapshot: Mutex<Option<GpuDirectRenderSnapshot>>,
}

/// The latest successful execution of one direct page callback, after GPUI clipping.
#[derive(Clone, Copy, Debug)]
pub struct GpuDirectRenderSnapshot {
    pub camera_serial: u64,
    pub visible_region: GpuPageRegion,
    pub root_transform: [f64; 6],
    pub target_size: [u32; 2],
    pub target_origin: [u32; 2],
    pub stats: GpuRenderStats,
}

/// Cloneable receipt that reports what happened to one direct GPUI draw.
#[derive(Clone)]
pub struct GpuDirectRenderReceipt {
    state: Arc<DirectRenderReceiptState>,
}

impl GpuDirectRenderReceipt {
    #[must_use]
    pub fn camera_serial(&self) -> u64 {
        self.state.camera_serial
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.state.generation
    }

    #[must_use]
    pub fn outcome(&self) -> GpuDirectRenderOutcome {
        GpuDirectRenderOutcome::from_u8(self.state.outcome.load(Ordering::Acquire))
    }

    fn record_execution(
        &self,
        outcome: GpuDirectRenderOutcome,
        snapshot: Option<GpuDirectRenderSnapshot>,
    ) -> bool {
        // Publish each execution before its first completion. The completion
        // counters remain once-per-submission, while reused callbacks stay observable.
        *self
            .state
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot;
        self.state
            .outcome
            .compare_exchange(
                GpuDirectRenderOutcome::Pending.as_u8(),
                outcome.as_u8(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Cached callbacks can execute again with a different GPUI content mask.
    /// Failed or superseded executions clear the previous successful snapshot.
    #[must_use]
    pub fn render_snapshot(&self) -> Option<GpuDirectRenderSnapshot> {
        if self.outcome() != GpuDirectRenderOutcome::Encoded {
            return None;
        }
        *self
            .state
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Painter callback and its completion receipt.
pub struct GpuDirectRenderSubmission {
    pub render: ExternalGpuRenderHandle,
    pub receipt: GpuDirectRenderReceipt,
}

impl DirectRenderTelemetry {
    fn schedule(&self, camera_serial: u64) {
        self.scheduled.fetch_add(1, Ordering::Relaxed);
        self.last_scheduled_camera_serial
            .store(camera_serial, Ordering::Release);
    }

    fn finish(
        &self,
        camera_serial: u64,
        outcome: GpuDirectRenderOutcome,
        stats: Option<([u32; 2], GpuRenderStats)>,
    ) {
        self.executed.fetch_add(1, Ordering::Relaxed);
        self.last_executed_camera_serial
            .store(camera_serial, Ordering::Release);
        match outcome {
            GpuDirectRenderOutcome::Encoded => {
                self.encoded.fetch_add(1, Ordering::Relaxed);
                self.last_encoded_camera_serial
                    .store(camera_serial, Ordering::Release);
            }
            GpuDirectRenderOutcome::Superseded => {
                self.superseded.fetch_add(1, Ordering::Relaxed);
            }
            GpuDirectRenderOutcome::Failed => {
                self.failed.fetch_add(1, Ordering::Relaxed);
            }
            GpuDirectRenderOutcome::Pending => {}
        }
        if let Some(stats) = stats {
            *self
                .latest_stats
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(stats);
        }
    }

    fn supersede(&self) {
        self.superseded.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> GpuDirectRenderStats {
        let latest = *self
            .latest_stats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (target_size, stats) = latest.unwrap_or_default();
        GpuDirectRenderStats {
            scheduled: self.scheduled.load(Ordering::Acquire),
            executed: self.executed.load(Ordering::Acquire),
            encoded: self.encoded.load(Ordering::Acquire),
            superseded: self.superseded.load(Ordering::Acquire),
            failed: self.failed.load(Ordering::Acquire),
            last_scheduled_camera_serial: self.last_scheduled_camera_serial.load(Ordering::Acquire),
            last_executed_camera_serial: self.last_executed_camera_serial.load(Ordering::Acquire),
            last_encoded_camera_serial: self.last_encoded_camera_serial.load(Ordering::Acquire),
            last_cpu_time: stats.cpu_time,
            last_frame_pool_wait_time: stats.frame_pool_wait_time,
            last_command_encoding_time: stats.command_encoding_time,
            last_frame_pool_grew: stats.frame_pool_grew,
            last_frame_pool_capacity_bytes: stats.frame_pool_capacity_bytes,
            last_target_size: target_size,
            last_passes_executed: stats.passes_executed,
            last_coverage_strategy: latest.map(|(_, stats)| stats.coverage_strategy),
            last_presentation_quality: latest.map(|(_, stats)| stats.presentation_quality),
            last_transient_peak_bytes: stats.transient_peak_bytes,
            last_transient_resident_bytes: stats.transient_resident_bytes,
            last_render_bundle_runs: stats.render_bundle_runs,
            last_stencil_draws: stats.stencil_draws,
            last_retained_stroke_candidate_references: stats.retained_stroke_candidate_references,
            last_retained_stroke_cull_dispatches: stats.retained_stroke_cull_dispatches,
            last_retained_stroke_indirect_draws: stats.retained_stroke_indirect_draws,
            last_retained_stroke_indirect_runs: stats.retained_stroke_indirect_runs,
            last_selected_stroke_lod: stats.selected_stroke_lod,
            last_active_stroke_lod_levels: stats.active_stroke_lod_levels,
            last_selected_stroke_segments: stats.selected_stroke_segments,
            last_target_visible_stroke_segments: stats.target_visible_stroke_segments,
            last_visible_stroke_tiles: stats.visible_stroke_tiles,
            last_max_visible_stroke_tile_segments: stats.max_visible_stroke_tile_segments,
        }
    }
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
        config.render_target_extent = RenderTargetExtentPolicy::RequestBounded;
        config.retained_strokes = RetainedStrokeStrategy::CameraAdaptive;
        let output_format = context.surface_format();
        config.output_format = output_format;
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
            motion_submitted: Arc::new(AtomicU64::new(0)),
            motion_completed: Arc::new(AtomicU64::new(0)),
            direct_render: DirectRenderTelemetry::default(),
        })
    }

    pub fn token(&self) -> ExternalGpuDeviceToken {
        self.context.token()
    }

    pub fn is_device_lost(&self) -> bool {
        self.context.is_device_lost()
    }

    pub fn max_texture_dimension_2d(&self) -> u32 {
        self.context.device().limits().max_texture_dimension_2d
    }

    pub fn direct_render_stats(&self) -> GpuDirectRenderStats {
        self.direct_render.snapshot()
    }

    /// Creates a painter-ordered callback that records this page directly into
    /// GPUI's current frame encoder.
    pub fn direct_render_handle(
        self: &Arc<Self>,
        page: Arc<PreparedGpuPage>,
        request: GpuPaintRequest,
    ) -> Result<GpuDirectRenderSubmission> {
        if self.is_device_lost() {
            bail!("the active GPUI WGPU device is lost");
        }
        if page.device_token() != self.token() {
            bail!("the prepared PDF page belongs to a stale GPU generation");
        }
        request.visible_region.clamp(request.page_size)?;
        self.direct_render.schedule(request.camera_serial);
        let receipt = GpuDirectRenderReceipt {
            state: Arc::new(DirectRenderReceiptState {
                camera_serial: request.camera_serial,
                generation: request.generation,
                outcome: AtomicU8::new(GpuDirectRenderOutcome::Pending.as_u8()),
                snapshot: Mutex::new(None),
            }),
        };
        let render = self.context.render_handle(Arc::new(DirectPageRender {
            renderer: Arc::clone(self),
            page,
            request,
            receipt: receipt.clone(),
        }));
        Ok(GpuDirectRenderSubmission { render, receipt })
    }

    /// Reserves one preallocated, non-presented target for the next sharp camera frame.
    ///
    /// `Ok(None)` is bounded backpressure: every camera target is still
    /// written or sampled. The caller should keep presenting its previous
    /// coherent texture and retry on the next animation frame.
    pub fn reserve_camera_target(
        &self,
        presentation: &GpuPagePresentation,
    ) -> Result<Option<GpuCameraTargetReservation>> {
        if self.is_device_lost() {
            bail!("the active GPUI WGPU device is lost");
        }
        if presentation
            .token
            .is_some_and(|token| token != self.token())
        {
            bail!("the PDF presentation belongs to a stale GPU generation");
        }
        let Some(size) = presentation.camera_target_size else {
            return Ok(None);
        };
        let completed_write = self.motion_completed.load(Ordering::Acquire);
        Ok(presentation.reserve_existing_camera_target(size, completed_write, self.token()))
    }

    fn mark_surface_write_submitted(&self, selection: &mut SurfaceSelection) {
        let reservation = selection
            .write_reservation
            .take()
            .expect("a rendered external texture owns its write reservation");
        let serial = self
            .motion_submitted
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        selection
            .surface
            .last_write_submission
            .store(serial, Ordering::Release);
        let completed = Arc::clone(&self.motion_completed);
        self.context.queue().on_submitted_work_done(move || {
            completed.fetch_max(serial, Ordering::Release);
        });
        // The write is already ordered on the shared queue. Releasing the
        // exclusive lease now allows a later GPUI scene sample to enqueue
        // behind it, while the per-surface serial still prevents another
        // writer from recycling this target before completion.
        reservation.mark_submitted();
        self.context.request_completion_poll();
    }

    /// Complete whole-page routing. No GPU resource is returned when either
    /// the frontend or renderer reports an unsupported semantic.
    pub fn prepare_page(
        &self,
        document: &GpuPdfDocument,
        page: u32,
        cancellation: &CancellationToken,
    ) -> Result<GpuPageRoute> {
        let total_started = Instant::now();
        let preflight_started = Instant::now();
        let frontend = document.frontend_diagnostics(page)?;
        let frontend_preflight = preflight_started.elapsed();
        if !frontend.is_empty() {
            return Ok(GpuPageRoute::Fallback(frontend.into()));
        }

        let compile_started = Instant::now();
        let (compiled, compiled_cache_hit) =
            document.compile_page_with_cache_status(page, cancellation)?;
        let compile = compile_started.elapsed();
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

        let link_started = Instant::now();
        let link_interactions = match compiled_link_interactions(&compiled) {
            Ok(interactions) => interactions,
            Err(diagnostic) => return Ok(GpuPageRoute::Fallback(Arc::from([diagnostic]))),
        };
        let warnings = compiled_page_warnings(&compiled);
        let link_conversion = link_started.elapsed();
        let gpu_started = Instant::now();
        let gpu = match self
            .renderer
            .prepare_interactive_with_cancellation(compiled.scene(), cancellation)
        {
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
        let gpu_prepare = gpu_started.elapsed();
        if !compiled_cache_hit {
            document.schedule_compiled_page_cache_write(Arc::clone(&compiled));
        }
        Ok(GpuPageRoute::Ready(Arc::new(PreparedGpuPage {
            compiled,
            gpu: Arc::new(gpu),
            link_interactions,
            warnings,
            timings: GpuPagePreparationTimings {
                frontend_preflight,
                compile,
                link_conversion,
                gpu_prepare,
                total: total_started.elapsed(),
                compiled_cache_hit,
            },
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

    /// Returns whether the retained vector error envelope is sufficient for
    /// this presentation without allocating or recording GPU work.
    pub fn vector_lod_status(
        &self,
        page: &PreparedGpuPage,
        request: GpuPaintRequest,
    ) -> Result<GpuVectorLodStatus> {
        if self.is_device_lost() {
            bail!("the active GPUI WGPU device is lost");
        }
        if page.device_token() != self.token() {
            bail!("the prepared PDF page belongs to a stale GPU generation");
        }
        let (_, root_transform) = self.render_root_transform(page, request)?;
        self.renderer
            .vector_lod_status(&page.gpu, root_transform)
            .map_err(render_error)
    }

    /// Builds a stricter complete retained page on a background worker.
    /// The caller keeps presenting `page` and swaps in this result only after
    /// successful completion, so resource pressure or cancellation cannot
    /// invalidate coherent pixels already on screen.
    pub fn prepare_vector_lod(
        &self,
        page: &PreparedGpuPage,
        scale: f32,
        cancellation: &CancellationToken,
    ) -> Result<Arc<PreparedGpuPage>> {
        if self.is_device_lost() {
            bail!("the active GPUI WGPU device is lost");
        }
        if page.device_token() != self.token() {
            bail!("the prepared PDF page belongs to a stale GPU generation");
        }
        let started = Instant::now();
        let gpu = self
            .renderer
            .prepare_interactive_at_scale_with_cancellation(
                page.compiled.scene(),
                scale,
                cancellation,
            )
            .map_err(|error| anyhow!(error).context("failed to prepare native PDF vector LOD"))?;
        let mut timings = page.timings;
        let elapsed = started.elapsed();
        timings.gpu_prepare = elapsed;
        timings.total = timings.total.saturating_add(elapsed);
        Ok(Arc::new(PreparedGpuPage {
            compiled: Arc::clone(&page.compiled),
            gpu: Arc::new(gpu),
            link_interactions: Arc::clone(&page.link_interactions),
            warnings: Arc::clone(&page.warnings),
            timings,
        }))
    }

    /// Invalidates older queued or currently encoding requests for `page`.
    pub fn supersede_page_render(&self, page: &PreparedGpuPage, generation: u64) -> Result<()> {
        if self.is_device_lost() {
            bail!("the active GPUI WGPU device is lost");
        }
        if page.device_token() != self.token() {
            bail!("the prepared PDF page belongs to a stale GPU generation");
        }
        self.renderer
            .supersede_render(&page.gpu, generation)
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
        let surface_translation = camera_surface_translation(surface_region, request)?;
        Ok((surface_region, surface_translation.compose(page_transform)))
    }

    fn engine_render_request_for_region(
        &self,
        page: &PreparedGpuPage,
        request: GpuPaintRequest,
        surface_region: GpuPageRegion,
        target_size: [u32; 2],
    ) -> Result<(GpuPageRegion, RenderRequest)> {
        let page_transform =
            page_transform(page.gpu.page_bounds(), request.rotation, request.page_size)?;
        let surface_translation = camera_surface_translation(surface_region, request)?;
        let root_transform = surface_translation.compose(page_transform);
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
        Ok((
            surface_region,
            RenderRequest {
                root_transform,
                viewport,
                target_size,
                background: request.background,
                settled: request.settled,
                generation: request.generation,
            },
        ))
    }

    /// Classify the next page update without allocating a texture, encoding
    /// commands, submitting work, or advancing the render generation.
    pub fn plan_page(
        &self,
        page: &PreparedGpuPage,
        request: GpuPaintRequest,
        presentation: &mut GpuPagePresentation,
    ) -> Result<IncrementalWorkPlan> {
        if self.is_device_lost() {
            bail!("the active GPUI WGPU device is lost");
        }
        if page.device_token() != self.token()
            || presentation
                .token
                .is_some_and(|token| token != self.token())
        {
            bail!("the prepared PDF page or presentation belongs to a stale GPU generation");
        }
        request.preallocated_camera_target_size(
            self.context.device().limits().max_texture_dimension_2d,
        )?;
        let mut region_mode = presentation.region_mode;
        loop {
            let surface_region = request.surface_region_for(region_mode)?;
            let target_size = request.target_size_for(surface_region, region_mode)?;
            let (_, engine_request) =
                self.engine_render_request_for_region(page, request, surface_region, target_size)?;
            presentation.prepare_request(engine_request);
            match self.renderer.plan_incremental(
                &page.gpu,
                &presentation.incremental,
                presentation.working_view(),
                engine_request,
            ) {
                Ok(work) => {
                    presentation.region_mode = region_mode;
                    return Ok(work);
                }
                Err(error) if render_target_limit(&error) => {
                    let Some(tighter) = region_mode.tighter() else {
                        return Err(render_error(error));
                    };
                    region_mode = tighter;
                }
                Err(error) => return Err(render_error(error)),
            }
        }
    }

    /// Render a prepared complete page into a bounded seven-surface ring.
    ///
    /// Reprojection and refinement select a destination distinct from their
    /// source. A direct retained redraw samples no presentation pixels and may
    /// reuse the current surface when its extent matches. The ring and
    /// incremental state are committed only after a successful submission, so
    /// callers can keep presenting the previous texture when this method fails.
    pub fn render_page(
        &self,
        page: &PreparedGpuPage,
        request: GpuPaintRequest,
        options: GpuPageRenderOptions,
        presentation: &mut GpuPagePresentation,
    ) -> Result<GpuPageRenderOutcome> {
        if self.is_device_lost() {
            bail!("the active GPUI WGPU device is lost");
        }
        if page.device_token() != self.token()
            || presentation
                .token
                .is_some_and(|token| token != self.token())
        {
            bail!("the prepared PDF page or presentation belongs to a stale GPU generation");
        }
        let camera_target_size = request.preallocated_camera_target_size(
            self.context.device().limits().max_texture_dimension_2d,
        )?;
        presentation.token = Some(self.token());
        let mut region_mode = presentation.region_mode;
        loop {
            let surface_region = request.surface_region_for(region_mode)?;
            let target_size = request.target_size_for(surface_region, region_mode)?;
            let (_, engine_request) =
                self.engine_render_request_for_region(page, request, surface_region, target_size)?;
            presentation.prepare_request(engine_request);
            let [width, height] = target_size;
            if width == 0 || height == 0 {
                bail!("native PDF render target must be non-empty");
            }
            let limit = self.context.device().limits().max_texture_dimension_2d;
            if width > limit || height > limit {
                let Some(tighter) = region_mode.tighter() else {
                    bail!("native PDF render target {width}x{height} exceeds device limit {limit}");
                };
                region_mode = tighter;
                continue;
            }
            let work = match self.renderer.plan_incremental(
                &page.gpu,
                &presentation.incremental,
                presentation.working_view(),
                engine_request,
            ) {
                Ok(work) => work,
                Err(error) if render_target_limit(&error) => {
                    let Some(tighter) = region_mode.tighter() else {
                        return Err(render_error(error));
                    };
                    region_mode = tighter;
                    continue;
                }
                Err(error) => return Err(render_error(error)),
            };
            if options.defer_full_render && work.kind == GpuIncrementalWorkKind::Full {
                // Preserve a tighter mode learned while probing device/render
                // target limits even though no presentation allocation occurs.
                presentation.region_mode = region_mode;
                return Ok(GpuPageRenderOutcome::Deferred);
            }
            let mut selection = presentation.select_target(
                target_size,
                work.kind == GpuIncrementalWorkKind::Direct,
                |size| self.create_surface(size),
            );
            let target = selection.surface();
            let mut incremental = presentation.incremental.clone();
            match self.renderer.render_incremental(
                &page.gpu,
                &mut incremental,
                presentation.working_view(),
                engine_request,
                &target.view,
            ) {
                Ok(stats) => {
                    self.mark_surface_write_submitted(&mut selection);
                    presentation.region_mode = region_mode;
                    let presented = presentation.commit(
                        selection,
                        surface_region,
                        incremental,
                        engine_request,
                        stats.presentation_quality,
                    );
                    if let Some(size) = camera_target_size {
                        // Allocate the viewport-sized camera target ring
                        // before interaction. A camera worker must never create
                        // a target or overwrite the coherent surface currently
                        // sampled by GPUI.
                        presentation.ensure_camera_targets(size, |size| self.create_surface(size));
                    }
                    if request.target_capacity.is_some()
                        && region_mode == SurfaceRegionMode::Overscanned
                    {
                        // Keep the current exact-density pan surface plus two
                        // alternates resident before interaction. A cache
                        // refresh must not create or zero a large texture on
                        // the gesture path, and the third surface lets GPUI
                        // finish sampling the retiring cache independently.
                        presentation.ensure_pan_cache_targets(target_size, |size| {
                            self.create_surface(size)
                        });
                    }
                    return Ok(GpuPageRenderOutcome::Rendered { presented, stats });
                }
                Err(RenderError::StaleRenderGeneration { .. }) => {
                    return Ok(GpuPageRenderOutcome::Superseded);
                }
                Err(error) if render_target_limit(&error) => {
                    let Some(tighter) = region_mode.tighter() else {
                        return Err(render_error(error));
                    };
                    region_mode = tighter;
                }
                Err(error) => return Err(render_error(error)),
            }
        }
    }

    /// Redraw one in-motion page directly from retained GPU resources.
    ///
    /// This path deliberately discards incremental presentation pixels and
    /// writes only to an existing non-presented camera target. Missing
    /// preallocation is an error: no texture allocation is permitted on the
    /// gesture path.
    pub fn render_camera_page(
        &self,
        page: &PreparedGpuPage,
        request: GpuPaintRequest,
        reservation: GpuCameraTargetReservation,
        presentation: &mut GpuPagePresentation,
    ) -> Result<GpuPageRenderOutcome> {
        if self.is_device_lost() {
            bail!("the active GPUI WGPU device is lost");
        }
        if page.device_token() != self.token()
            || presentation
                .token
                .is_some_and(|token| token != self.token())
        {
            bail!("the prepared PDF page or presentation belongs to a stale GPU generation");
        }

        let target_size = request
            .preallocated_camera_target_size(
                self.context.device().limits().max_texture_dimension_2d,
            )?
            .ok_or_else(|| anyhow!("native PDF camera target was not preallocated"))?;
        if reservation.token != self.token() || reservation.size != target_size {
            bail!("native PDF camera target reservation is stale or has the wrong extent");
        }
        let SurfaceSlot::Existing(reserved_index) = reservation.selection.slot else {
            bail!("native PDF camera target reservation does not reference an existing slot");
        };
        if !presentation
            .surfaces
            .get(reserved_index)
            .is_some_and(|surface| Arc::ptr_eq(surface, &reservation.selection.surface))
        {
            bail!("native PDF camera target reservation no longer belongs to this presentation");
        }
        let surface_region = request.visible_region.clamp(request.page_size)?;
        let render_size = surface_region.size();
        let (_, engine_request) =
            self.engine_render_request_for_region(page, request, surface_region, render_size)?;
        let [width, height] = target_size;
        if width == 0 || height == 0 {
            bail!("native PDF render target must be non-empty");
        }
        let limit = self.context.device().limits().max_texture_dimension_2d;
        if width > limit || height > limit {
            bail!("native PDF render target {width}x{height} exceeds device limit {limit}");
        }

        // Work on a clone so a pre-submission error leaves all presentation
        // metadata intact. The coherent presented target remains protected
        // until the replacement submission succeeds.
        let mut next = presentation.clone();
        next.token = Some(self.token());
        next.incremental.clear();
        next.current = next.presented;
        next.current_region = next.presented_region;
        let mut selection = reservation.selection;
        let target = selection.surface();
        match self
            .renderer
            .render(&page.gpu, engine_request, &target.view)
        {
            Ok(stats) => {
                self.mark_surface_write_submitted(&mut selection);
                let presented = next.commit(
                    selection,
                    surface_region,
                    IncrementalRenderState::default(),
                    engine_request,
                    stats.presentation_quality,
                );
                *presentation = next;
                Ok(GpuPageRenderOutcome::Rendered { presented, stats })
            }
            Err(RenderError::StaleRenderGeneration { .. }) => Ok(GpuPageRenderOutcome::Superseded),
            Err(error) => Err(render_error(error)),
        }
    }

    fn create_surface(&self, size: [u32; 2]) -> Arc<GpuPageSurface> {
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
        Arc::new(GpuPageSurface {
            _texture: texture,
            view,
            handle,
            size,
            token: self.token(),
            last_write_submission: AtomicU64::new(0),
        })
    }
}

struct DirectPageRender {
    renderer: Arc<GpuPdfRenderer>,
    page: Arc<PreparedGpuPage>,
    request: GpuPaintRequest,
    receipt: GpuDirectRenderReceipt,
}

impl DirectPageRender {
    fn finish(&self, outcome: GpuDirectRenderOutcome, snapshot: Option<GpuDirectRenderSnapshot>) {
        if self.receipt.record_execution(outcome, snapshot) {
            self.renderer.direct_render.finish(
                self.request.camera_serial,
                outcome,
                snapshot.map(|snapshot| (snapshot.target_size, snapshot.stats)),
            );
        }
    }
}

impl Drop for DirectPageRender {
    fn drop(&mut self) {
        if self
            .receipt
            .state
            .outcome
            .compare_exchange(
                GpuDirectRenderOutcome::Pending.as_u8(),
                GpuDirectRenderOutcome::Superseded.as_u8(),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.renderer.direct_render.supersede();
        }
    }
}

fn direct_render_encoded<T>(result: std::result::Result<T, RenderError>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(RenderError::StaleRenderGeneration { .. }) => Ok(None),
        Err(error) => Err(render_error(error)),
    }
}

impl WgpuExternalRender for DirectPageRender {
    fn render(&self, context: &mut WgpuExternalRenderContext<'_>) -> Result<()> {
        let result = (|| -> Result<Option<GpuDirectRenderSnapshot>> {
            if context.target_format != self.renderer.output_format
                || !output_format_supported(context.target_format)
            {
                bail!(
                    "native PDF direct target format {:?} does not match renderer format {:?}",
                    context.target_format,
                    self.renderer.output_format
                );
            }
            let Some((target_origin, visible_region)) = clipped_direct_region(
                [
                    context.bounds.origin.x.as_f32(),
                    context.bounds.origin.y.as_f32(),
                    context.bounds.size.width.as_f32(),
                    context.bounds.size.height.as_f32(),
                ],
                [
                    context.content_mask.bounds.origin.x.as_f32(),
                    context.content_mask.bounds.origin.y.as_f32(),
                    context.content_mask.bounds.size.width.as_f32(),
                    context.content_mask.bounds.size.height.as_f32(),
                ],
                context.target_size,
                self.request.visible_region,
                self.request.page_size,
            ) else {
                return Ok(None);
            };
            let mut request = self.request;
            request.visible_region = visible_region;
            let (_, engine_request) = self.renderer.engine_render_request_for_region(
                &self.page,
                request,
                visible_region,
                visible_region.size(),
            )?;
            let frame_capacity = self
                .request
                .preallocated_camera_target_size(
                    self.renderer
                        .context
                        .device()
                        .limits()
                        .max_texture_dimension_2d,
                )?
                .unwrap_or(engine_request.target_size);
            direct_render_encoded(
                self.renderer
                    .renderer
                    .encode_into_region_with_frame_capacity(
                        &self.page.gpu,
                        engine_request,
                        context.target,
                        target_origin,
                        frame_capacity,
                        context.encoder,
                    ),
            )
            .map(|stats| {
                stats.map(|stats| GpuDirectRenderSnapshot {
                    camera_serial: request.camera_serial,
                    visible_region,
                    root_transform: engine_request.root_transform.coefficients(),
                    target_size: engine_request.target_size,
                    target_origin,
                    stats,
                })
            })
        })();
        let outcome = match result {
            Ok(Some(_)) => GpuDirectRenderOutcome::Encoded,
            Ok(None) => GpuDirectRenderOutcome::Superseded,
            Err(_) => GpuDirectRenderOutcome::Failed,
        };
        self.finish(outcome, result.as_ref().ok().copied().flatten());
        result.map(|_| ())
    }
}

fn clipped_direct_region(
    bounds: [f32; 4],
    mask: [f32; 4],
    target_size: [u32; 2],
    visible: GpuPageRegion,
    page_size: [u32; 2],
) -> Option<([u32; 2], GpuPageRegion)> {
    if bounds
        .into_iter()
        .chain(mask)
        .any(|value| !value.is_finite())
    {
        return None;
    }
    let [x, y, width, height] = bounds;
    let [mask_x, mask_y, mask_width, mask_height] = mask;
    let original_x = x.round() as i64;
    let original_y = y.round() as i64;
    let x0 = original_x
        .max(mask_x.round() as i64)
        .max(0)
        .min(i64::from(target_size[0]));
    let y0 = original_y
        .max(mask_y.round() as i64)
        .max(0)
        .min(i64::from(target_size[1]));
    let x1 = (x + width)
        .round()
        .min((mask_x + mask_width).round())
        .clamp(0.0, target_size[0] as f32) as i64;
    let y1 = (y + height)
        .round()
        .min((mask_y + mask_height).round())
        .clamp(0.0, target_size[1] as f32) as i64;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let offset_x = u32::try_from(x0.saturating_sub(original_x)).ok()?;
    let offset_y = u32::try_from(y0.saturating_sub(original_y)).ok()?;
    let region_x = visible.x.checked_add(offset_x)?.min(page_size[0]);
    let region_y = visible.y.checked_add(offset_y)?.min(page_size[1]);
    let region = GpuPageRegion {
        x: region_x,
        y: region_y,
        width: u32::try_from(x1 - x0)
            .ok()?
            .min(page_size[0].saturating_sub(region_x)),
        height: u32::try_from(y1 - y0)
            .ok()?
            .min(page_size[1].saturating_sub(region_y)),
    };
    (region.width > 0 && region.height > 0).then_some(([x0 as u32, y0 as u32], region))
}

fn renderer_init_error(error: RendererInitError) -> anyhow::Error {
    anyhow!(error).context("failed to initialize native PDF GPU renderer")
}

fn render_error(error: RenderError) -> anyhow::Error {
    anyhow!(error).context("failed to execute native PDF GPU render")
}

fn render_target_limit(error: &RenderError) -> bool {
    matches!(
        error,
        RenderError::Limit { resource, .. }
            if resource.contains("transient") || resource.contains("frame-pool")
    )
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

    fn with_gutter_aligned(
        self,
        page_size: [u32; 2],
        gutter: [u32; 2],
        alignment: u32,
    ) -> Result<Self> {
        if alignment == 0 {
            bail!("native PDF surface alignment must be non-zero");
        }
        let visible = self.clamp(page_size)?;
        let x0 = visible.x.saturating_sub(gutter[0]) / alignment * alignment;
        let y0 = visible.y.saturating_sub(gutter[1]) / alignment * alignment;
        let x1 = visible
            .right()
            .saturating_add(gutter[0])
            .min(page_size[0])
            .div_ceil(alignment)
            .saturating_mul(alignment)
            .min(page_size[0]);
        let y1 = visible
            .bottom()
            .saturating_add(gutter[1])
            .min(page_size[1])
            .div_ceil(alignment)
            .saturating_mul(alignment)
            .min(page_size[1]);
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
    /// Stable allocation extent for an overscanned interactive surface. The
    /// rendered page subregion occupies its top-left corner and presentation
    /// crops the unused capacity. Tighter budget-recovery modes ignore it.
    pub target_capacity: Option<[u32; 2]>,
    /// Axes receiving the full 1.7x pan-cache overscan. An inactive cross axis
    /// retains the 96-device-pixel minimum border.
    pub pan_cache_axes: [bool; 2],
    /// Active axes promoted to the complete page extent when the retained
    /// scene is expensive and the bounded seven-target ring can afford it.
    pub full_page_pan_cache_axes: [bool; 2],
    /// Preallocated viewport-sized target used for a direct retained camera
    /// redraw. It intentionally excludes pan-cache overscan.
    pub camera_target_capacity: Option<[u32; 2]>,
    /// Monotonic reader camera update represented by this request. The value
    /// does not affect pixels, but lets the reader prove that an accepted pan
    /// or zoom state reached the same-device presentation ring.
    pub camera_serial: u64,
    /// Fractional device-pixel offset between the continuous page origin and
    /// GPUI's snapped external-texture destination. Applying it to the root
    /// transform keeps retained vector coverage phase-aligned with the camera.
    pub device_origin_phase: [f32; 2],
    pub rotation: DocumentRotation,
    pub background: [f64; 4],
    pub settled: bool,
    pub generation: u64,
}

impl GpuPaintRequest {
    pub fn surface_region(self) -> Result<GpuPageRegion> {
        self.surface_region_for(SurfaceRegionMode::Overscanned)
    }

    fn surface_region_for(self, mode: SurfaceRegionMode) -> Result<GpuPageRegion> {
        if mode == SurfaceRegionMode::Visible {
            return self.visible_region.clamp(self.page_size);
        }
        let gutter = [
            if mode == SurfaceRegionMode::Overscanned {
                pan_cache_gutter(self.visible_region.width, self.pan_cache_axes[0])
            } else {
                0
            },
            if mode == SurfaceRegionMode::Overscanned {
                pan_cache_gutter(self.visible_region.height, self.pan_cache_axes[1])
            } else {
                0
            },
        ];
        let mut region =
            self.visible_region
                .with_gutter_aligned(self.page_size, gutter, ANALYTIC_TILE_SIZE)?;
        if mode == SurfaceRegionMode::Overscanned {
            if self.full_page_pan_cache_axes[0] {
                region.x = 0;
                region.width = self.page_size[0];
            }
            if self.full_page_pan_cache_axes[1] {
                region.y = 0;
                region.height = self.page_size[1];
            }
        }
        Ok(region)
    }

    fn target_size_for(
        self,
        surface_region: GpuPageRegion,
        mode: SurfaceRegionMode,
    ) -> Result<[u32; 2]> {
        let region_size = surface_region.size();
        let Some(capacity) = self
            .target_capacity
            .filter(|_| mode == SurfaceRegionMode::Overscanned)
        else {
            return Ok(region_size);
        };
        if capacity.contains(&0) {
            bail!("native PDF stable target capacity must be non-empty");
        }
        Ok([
            capacity[0].max(region_size[0]),
            capacity[1].max(region_size[1]),
        ])
    }

    fn preallocated_camera_target_size(self, dimension_limit: u32) -> Result<Option<[u32; 2]>> {
        let Some(capacity) = self.camera_target_capacity else {
            return Ok(None);
        };
        if capacity.contains(&0) {
            bail!("native PDF camera target capacity must be non-empty");
        }
        if capacity[0] > dimension_limit || capacity[1] > dimension_limit {
            bail!(
                "native PDF camera target capacity {}x{} exceeds device limit {dimension_limit}",
                capacity[0],
                capacity[1]
            );
        }
        let visible_size = self.visible_region.clamp(self.page_size)?.size();
        let target_size = [
            capacity[0].max(visible_size[0]),
            capacity[1].max(visible_size[1]),
        ];
        if target_size[0] > dimension_limit || target_size[1] > dimension_limit {
            bail!(
                "native PDF camera render target {}x{} exceeds device limit {dimension_limit}",
                target_size[0],
                target_size[1]
            );
        }
        Ok(Some(target_size))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SurfaceRegionMode {
    #[default]
    Overscanned,
    TileAligned,
    Visible,
}

impl SurfaceRegionMode {
    const fn tighter(self) -> Option<Self> {
        match self {
            Self::Overscanned => Some(Self::TileAligned),
            Self::TileAligned => Some(Self::Visible),
            Self::Visible => None,
        }
    }
}

struct GpuPageSurface {
    _texture: Arc<wgpu::Texture>,
    view: Arc<wgpu::TextureView>,
    handle: ExternalTextureHandle,
    size: [u32; 2],
    token: ExternalGpuDeviceToken,
    last_write_submission: AtomicU64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SurfaceSlot {
    Existing(usize),
    Insert,
    Replace(usize),
}

struct SurfaceSelection {
    slot: SurfaceSlot,
    surface: Arc<GpuPageSurface>,
    write_reservation: Option<ExternalTextureWriteReservation>,
}

impl SurfaceSelection {
    fn surface(&self) -> &GpuPageSurface {
        &self.surface
    }
}

/// One safely reserved, preallocated destination for a retained motion redraw.
///
/// The reservation is tied to the presentation clone that supplied the slot.
/// Dropping it before rendering releases the external-texture write lease.
pub struct GpuCameraTargetReservation {
    selection: SurfaceSelection,
    size: [u32; 2],
    token: ExternalGpuDeviceToken,
}

impl fmt::Debug for GpuCameraTargetReservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuCameraTargetReservation")
            .field("slot", &self.selection.slot)
            .field("size", &self.size)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

/// Bounded same-device presentation state for one prepared page.
///
/// Working state may advance through incomplete settled tiles while the last
/// coherent surface remains immutable and displayable. At most seven surfaces
/// include three exact-density pan-cache slots and two camera targets.
/// Completion-gated double buffering prevents queued PDF frames from
/// starving the compositor while keeping allocation off the gesture path.
#[derive(Clone, Default)]
pub struct GpuPagePresentation {
    token: Option<ExternalGpuDeviceToken>,
    surfaces: Vec<Arc<GpuPageSurface>>,
    current: Option<usize>,
    current_region: Option<GpuPageRegion>,
    working_request: Option<RenderRequest>,
    working_quality: GpuPresentationQuality,
    presented: Option<usize>,
    presented_region: Option<GpuPageRegion>,
    presented_request: Option<RenderRequest>,
    presented_quality: Option<GpuPresentationQuality>,
    presented_incremental: IncrementalRenderState,
    camera_target_size: Option<[u32; 2]>,
    region_mode: SurfaceRegionMode,
    incremental: IncrementalRenderState,
}

impl fmt::Debug for GpuPagePresentation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuPagePresentation")
            .field("token", &self.token)
            .field("surfaces", &self.surfaces.len())
            .field("working", &self.current)
            .field("working_region", &self.current_region)
            .field("working_quality", &self.working_quality)
            .field("presented", &self.presented)
            .field("presented_region", &self.presented_region)
            .field("presented_quality", &self.presented_quality)
            .field("camera_target_size", &self.camera_target_size)
            .field("region_mode", &self.region_mode)
            .field("refinement_pending", &self.incremental.refinement_pending())
            .finish()
    }
}

impl GpuPagePresentation {
    /// Chooses the stable presentation extent, including any budget-driven
    /// reduction learned from an earlier render of this page.
    pub fn surface_region(&self, request: GpuPaintRequest) -> Result<GpuPageRegion> {
        request.surface_region_for(self.region_mode)
    }

    fn working_view(&self) -> Option<&wgpu::TextureView> {
        self.current
            .and_then(|index| self.surfaces.get(index))
            .map(|surface| surface.view.as_ref())
    }

    fn ensure_camera_targets(
        &mut self,
        size: [u32; 2],
        mut create: impl FnMut([u32; 2]) -> Arc<GpuPageSurface>,
    ) -> usize {
        self.camera_target_size = Some(size);
        while self.available_camera_target_count(size) < CAMERA_TARGET_COUNT {
            let surface = create(size);
            if self.surfaces.len() < MAX_PRESENTATION_SURFACES {
                self.surfaces.push(surface);
                continue;
            }
            let Some(index) = self
                .unprotected_surface_indices()
                .find(|index| self.surfaces[*index].size != size)
            else {
                break;
            };
            self.surfaces[index] = surface;
        }
        self.available_camera_target_count(size)
    }

    fn ensure_pan_cache_targets(
        &mut self,
        size: [u32; 2],
        mut create: impl FnMut([u32; 2]) -> Arc<GpuPageSurface>,
    ) -> usize {
        while self
            .surfaces
            .iter()
            .filter(|surface| surface.size == size)
            .count()
            < PAN_CACHE_TARGET_COUNT
        {
            let surface = create(size);
            if self.surfaces.len() < MAX_PRESENTATION_SURFACES {
                self.surfaces.push(surface);
                continue;
            }
            let Some(index) = self
                .unprotected_surface_indices()
                .find(|index| self.surfaces[*index].size != size)
            else {
                break;
            };
            self.surfaces[index] = surface;
        }
        self.surfaces
            .iter()
            .filter(|surface| surface.size == size)
            .count()
    }

    #[must_use]
    pub const fn camera_target_size(&self) -> Option<[u32; 2]> {
        self.camera_target_size
    }

    fn prepare_request(&mut self, request: RenderRequest) {
        if self.working_quality == GpuPresentationQuality::Incomplete
            && self
                .working_request
                .is_some_and(|working| !same_render_content(working, request))
        {
            self.current = self.presented;
            self.current_region = self.presented_region;
            self.working_request = self.presented_request;
            self.working_quality = self
                .presented_quality
                .unwrap_or(GpuPresentationQuality::Incomplete);
            self.incremental = self.presented_incremental.clone();
        }
    }

    fn target_slot(&self, size: [u32; 2], allow_current: bool) -> SurfaceSlot {
        let working_retained = self
            .surfaces
            .iter()
            .position(|surface| self.incremental.references_texture(&surface.view));
        let presented_retained = self
            .surfaces
            .iter()
            .position(|surface| self.presented_incremental.references_texture(&surface.view));
        let protected = [self.presented, working_retained, presented_retained];
        let mut sizes = [[0_u32; 2]; MAX_PRESENTATION_SURFACES];
        for (output, surface) in sizes.iter_mut().zip(&self.surfaces) {
            *output = surface.size;
        }
        presentation_surface_slot(
            &sizes[..self.surfaces.len()],
            self.current,
            &protected,
            size,
            allow_current,
        )
    }

    fn unprotected_surface_indices(&self) -> impl Iterator<Item = usize> + '_ {
        let working_retained = self
            .surfaces
            .iter()
            .position(|surface| self.incremental.references_texture(&surface.view));
        let presented_retained = self
            .surfaces
            .iter()
            .position(|surface| self.presented_incremental.references_texture(&surface.view));
        let protected = [
            self.current,
            self.presented,
            working_retained,
            presented_retained,
        ];
        (0..self.surfaces.len()).filter(move |index| !protected.contains(&Some(*index)))
    }

    fn available_camera_target_count(&self, size: [u32; 2]) -> usize {
        self.unprotected_surface_indices()
            .filter(|index| self.surfaces[*index].size == size)
            .count()
    }

    fn reserve_existing_camera_target(
        &self,
        size: [u32; 2],
        completed_write: u64,
        token: ExternalGpuDeviceToken,
    ) -> Option<GpuCameraTargetReservation> {
        self.unprotected_surface_indices().find_map(|index| {
            let surface = &self.surfaces[index];
            if surface.size != size
                || surface.last_write_submission.load(Ordering::Acquire) > completed_write
            {
                return None;
            }
            let write_reservation = surface.handle.try_reserve_write()?;
            Some(GpuCameraTargetReservation {
                selection: SurfaceSelection {
                    slot: SurfaceSlot::Existing(index),
                    surface: Arc::clone(surface),
                    write_reservation: Some(write_reservation),
                },
                size,
                token,
            })
        })
    }

    fn select_target(
        &self,
        size: [u32; 2],
        allow_current: bool,
        create: impl FnOnce([u32; 2]) -> Arc<GpuPageSurface>,
    ) -> SurfaceSelection {
        let slot = self.target_slot(size, allow_current);
        if let SurfaceSlot::Existing(index) = slot {
            let surface = Arc::clone(&self.surfaces[index]);
            if let Some(write_reservation) = surface.handle.try_reserve_write() {
                return SurfaceSelection {
                    slot,
                    surface,
                    write_reservation: Some(write_reservation),
                };
            }
            // The logical slot is reusable, but a previously submitted GPUI
            // scene still samples its texture. Replace the Arc in the cloned
            // presentation instead of racing that sample.
            let surface = create(size);
            let write_reservation = surface
                .handle
                .try_reserve_write()
                .expect("a newly created external texture is writable");
            return SurfaceSelection {
                slot: SurfaceSlot::Replace(index),
                surface,
                write_reservation: Some(write_reservation),
            };
        }
        let surface = create(size);
        let write_reservation = surface
            .handle
            .try_reserve_write()
            .expect("a newly created external texture is writable");
        SurfaceSelection {
            slot,
            surface,
            write_reservation: Some(write_reservation),
        }
    }

    fn commit(
        &mut self,
        selection: SurfaceSelection,
        region: GpuPageRegion,
        incremental: IncrementalRenderState,
        request: RenderRequest,
        quality: GpuPresentationQuality,
    ) -> Option<GpuPageTexture> {
        debug_assert!(selection.write_reservation.is_none());
        let index = match selection.slot {
            SurfaceSlot::Existing(index) => index,
            SurfaceSlot::Insert => {
                let index = self.surfaces.len();
                self.surfaces.push(Arc::clone(&selection.surface));
                index
            }
            SurfaceSlot::Replace(index) => {
                self.surfaces[index] = Arc::clone(&selection.surface);
                index
            }
        };
        self.current = Some(index);
        self.current_region = Some(region);
        self.working_request = Some(request);
        self.working_quality = quality;
        self.incremental = incremental;
        if quality == GpuPresentationQuality::Incomplete {
            return None;
        }
        self.presented = Some(index);
        self.presented_region = Some(region);
        self.presented_request = Some(request);
        self.presented_quality = Some(quality);
        self.presented_incremental = self.incremental.clone();
        Some(GpuPageTexture {
            surface: selection.surface,
            region,
            source_region: GpuPageRegion {
                x: 0,
                y: 0,
                width: region.width,
                height: region.height,
            },
        })
    }

    #[must_use]
    pub fn current_texture(&self) -> Option<GpuPageTexture> {
        let surface = Arc::clone(self.surfaces.get(self.presented?)?);
        Some(GpuPageTexture {
            surface,
            region: self.presented_region?,
            source_region: GpuPageRegion {
                x: 0,
                y: 0,
                width: self.presented_region?.width,
                height: self.presented_region?.height,
            },
        })
    }

    #[must_use]
    pub const fn has_working_texture(&self) -> bool {
        self.current.is_some()
    }

    #[must_use]
    pub const fn presentation_quality(&self) -> Option<GpuPresentationQuality> {
        self.presented_quality
    }

    #[must_use]
    pub fn projected_byte_len(&self, region: GpuPageRegion, allow_current: bool) -> u64 {
        let mut bytes = self
            .surfaces
            .iter()
            .map(|surface| surface_byte_len(surface.size))
            .sum::<u64>();
        match self.target_slot(region.size(), allow_current) {
            SurfaceSlot::Existing(_) => {}
            SurfaceSlot::Insert => bytes = bytes.saturating_add(region.byte_len()),
            SurfaceSlot::Replace(index) => {
                bytes = bytes
                    .saturating_sub(surface_byte_len(self.surfaces[index].size))
                    .saturating_add(region.byte_len());
            }
        }
        for incremental in [&self.incremental, &self.presented_incremental] {
            if incremental.retained_texture_bytes() > 0
                && !self
                    .surfaces
                    .iter()
                    .any(|surface| incremental.references_texture(&surface.view))
            {
                bytes = bytes.saturating_add(incremental.retained_texture_bytes());
            }
        }
        bytes
    }

    #[must_use]
    pub fn byte_len(&self) -> u64 {
        self.surfaces
            .iter()
            .map(|surface| surface_byte_len(surface.size))
            .sum::<u64>()
    }

    #[must_use]
    pub fn surface_count(&self) -> usize {
        self.surfaces.len()
    }

    #[must_use]
    pub fn retained_source_byte_len(&self) -> u64 {
        let working = self.incremental.retained_texture_bytes();
        let presented = self.presented_incremental.retained_texture_bytes();
        let shared = self.surfaces.iter().any(|surface| {
            self.incremental.references_texture(&surface.view)
                && self.presented_incremental.references_texture(&surface.view)
        });
        if shared {
            working.max(presented)
        } else {
            working.saturating_add(presented)
        }
    }

    #[must_use]
    pub fn refinement_pending(&self) -> bool {
        self.incremental.refinement_pending()
    }

    #[must_use]
    pub const fn token(&self) -> Option<ExternalGpuDeviceToken> {
        self.token
    }
}

fn presentation_surface_slot(
    sizes: &[[u32; 2]],
    current: Option<usize>,
    protected: &[Option<usize>],
    requested: [u32; 2],
    allow_current: bool,
) -> SurfaceSlot {
    let protected = |index: usize| protected.contains(&Some(index));
    if allow_current
        && let Some(current) = current
        && !protected(current)
        && sizes.get(current) == Some(&requested)
    {
        return SurfaceSlot::Existing(current);
    }
    let reusable = |index: usize| Some(index) != current && !protected(index);
    if let Some(index) = sizes
        .iter()
        .enumerate()
        .find_map(|(index, size)| (reusable(index) && *size == requested).then_some(index))
    {
        return SurfaceSlot::Existing(index);
    }
    if sizes.len() < MAX_PRESENTATION_SURFACES {
        return SurfaceSlot::Insert;
    }
    let start = current.map_or(0, |index| index + 1);
    for offset in 0..sizes.len() {
        let index = (start + offset) % sizes.len();
        if reusable(index) {
            return SurfaceSlot::Replace(index);
        }
    }
    // Presented and incremental retained sources overlap in normal operation;
    // the seven-entry ring therefore always has one writable surface.
    unreachable!("native PDF presentation ring has no writable surface")
}

fn same_render_content(left: RenderRequest, right: RenderRequest) -> bool {
    left.root_transform == right.root_transform
        && left.target_size == right.target_size
        && left.background == right.background
        && left.settled == right.settled
}

fn surface_byte_len(size: [u32; 2]) -> u64 {
    u64::from(size[0])
        .saturating_mul(u64::from(size[1]))
        .saturating_mul(4)
}

/// Reusable render target and stable GPUI presentation handle.
#[derive(Clone)]
pub struct GpuPageTexture {
    surface: Arc<GpuPageSurface>,
    region: GpuPageRegion,
    source_region: GpuPageRegion,
}

impl fmt::Debug for GpuPageTexture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GpuPageTexture")
            .field("handle", &self.surface.handle)
            .field("size", &self.surface.size)
            .field("region", &self.region)
            .field("source_region", &self.source_region)
            .field("token", &self.surface.token)
            .finish_non_exhaustive()
    }
}

impl GpuPageTexture {
    pub fn handle(&self) -> &ExternalTextureHandle {
        &self.surface.handle
    }

    pub fn size(&self) -> [u32; 2] {
        self.surface.size
    }

    pub const fn region(&self) -> GpuPageRegion {
        self.region
    }

    /// Normalized extent of the subregion containing rendered page pixels.
    /// Any remaining allocation capacity is intentionally excluded.
    pub fn source_extent(&self) -> [f32; 2] {
        [
            self.source_region.width as f32 / self.surface.size[0] as f32,
            self.source_region.height as f32 / self.surface.size[1] as f32,
        ]
    }

    /// Normalized source rectangle inside the external texture.
    pub fn source_bounds(&self) -> [f32; 4] {
        [
            self.source_region.x as f32 / self.surface.size[0] as f32,
            self.source_region.y as f32 / self.surface.size[1] as f32,
            self.source_region.width as f32 / self.surface.size[0] as f32,
            self.source_region.height as f32 / self.surface.size[1] as f32,
        ]
    }

    pub fn byte_len(&self) -> u64 {
        surface_byte_len(self.surface.size)
    }

    pub fn token(&self) -> ExternalGpuDeviceToken {
        self.surface.token
    }
}

fn camera_surface_translation(
    surface_region: GpuPageRegion,
    request: GpuPaintRequest,
) -> Result<Transform> {
    let [phase_x, phase_y] = request.device_origin_phase;
    if !phase_x.is_finite()
        || !phase_y.is_finite()
        || phase_x.abs() > 0.500_001
        || phase_y.abs() > 0.500_001
    {
        bail!("native PDF device-origin phase must be finite and within half a device pixel");
    }
    Transform::new([
        1.0,
        0.0,
        0.0,
        1.0,
        -f64::from(surface_region.x) + f64::from(phase_x),
        -f64::from(surface_region.y) + f64::from(phase_y),
    ])
    .context("failed to construct native PDF camera surface translation")
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

    #[test]
    fn direct_render_telemetry_distinguishes_scheduling_from_execution() {
        let telemetry = DirectRenderTelemetry::default();
        telemetry.schedule(7);
        assert_eq!(
            telemetry.snapshot(),
            GpuDirectRenderStats {
                scheduled: 1,
                last_scheduled_camera_serial: 7,
                ..GpuDirectRenderStats::default()
            }
        );
        telemetry.finish(
            7,
            GpuDirectRenderOutcome::Encoded,
            Some((
                [1920, 1080],
                GpuRenderStats {
                    cpu_time: Duration::from_millis(3),
                    command_encoding_time: Duration::from_millis(2),
                    presentation_quality: GpuPresentationQuality::Exact,
                    passes_executed: 6,
                    frame_pool_capacity_bytes: 4096,
                    transient_peak_bytes: 3072,
                    transient_resident_bytes: 4096,
                    render_bundle_runs: 5,
                    stencil_draws: 7,
                    retained_stroke_candidate_references: 11,
                    retained_stroke_cull_dispatches: 2,
                    retained_stroke_indirect_draws: 3,
                    retained_stroke_indirect_runs: 1,
                    selected_stroke_lod: 2,
                    active_stroke_lod_levels: 4,
                    selected_stroke_segments: 37,
                    target_visible_stroke_segments: 50_000,
                    visible_stroke_tiles: 9,
                    max_visible_stroke_tile_segments: 7,
                    ..GpuRenderStats::default()
                },
            )),
        );
        telemetry.schedule(8);
        telemetry.finish(8, GpuDirectRenderOutcome::Failed, None);
        assert_eq!(
            telemetry.snapshot(),
            GpuDirectRenderStats {
                scheduled: 2,
                executed: 2,
                encoded: 1,
                failed: 1,
                last_scheduled_camera_serial: 8,
                last_executed_camera_serial: 8,
                last_encoded_camera_serial: 7,
                last_cpu_time: Duration::from_millis(3),
                last_command_encoding_time: Duration::from_millis(2),
                last_frame_pool_capacity_bytes: 4096,
                last_target_size: [1920, 1080],
                last_passes_executed: 6,
                last_coverage_strategy: Some(GpuVectorCoverageStrategy::RetainedTriangles),
                last_presentation_quality: Some(GpuPresentationQuality::Exact),
                last_transient_peak_bytes: 3072,
                last_transient_resident_bytes: 4096,
                last_render_bundle_runs: 5,
                last_stencil_draws: 7,
                last_retained_stroke_candidate_references: 11,
                last_retained_stroke_cull_dispatches: 2,
                last_retained_stroke_indirect_draws: 3,
                last_retained_stroke_indirect_runs: 1,
                last_selected_stroke_lod: 2,
                last_active_stroke_lod_levels: 4,
                last_selected_stroke_segments: 37,
                last_target_visible_stroke_segments: 50_000,
                last_visible_stroke_tiles: 9,
                last_max_visible_stroke_tile_segments: 7,
                ..GpuDirectRenderStats::default()
            }
        );
    }

    #[test]
    fn direct_render_receipt_exposes_request_identity_and_outcome() {
        let state = Arc::new(DirectRenderReceiptState {
            camera_serial: 9,
            generation: 4,
            outcome: AtomicU8::new(GpuDirectRenderOutcome::Pending.as_u8()),
            snapshot: Mutex::new(None),
        });
        let receipt = GpuDirectRenderReceipt { state };
        assert_eq!(receipt.camera_serial(), 9);
        assert_eq!(receipt.generation(), 4);
        assert_eq!(receipt.outcome(), GpuDirectRenderOutcome::Pending);
        assert!(receipt.render_snapshot().is_none());
        let mut snapshot = GpuDirectRenderSnapshot {
            camera_serial: 9,
            visible_region: GpuPageRegion {
                x: 0,
                y: 0,
                width: 32,
                height: 64,
            },
            root_transform: Transform::IDENTITY.coefficients(),
            target_size: [32, 64],
            target_origin: [10, 20],
            stats: GpuRenderStats {
                generation: 4,
                ..GpuRenderStats::default()
            },
        };
        let assert_snapshot = |expected: GpuDirectRenderSnapshot| {
            let actual = receipt.render_snapshot().expect("encoded render snapshot");
            assert_eq!(actual.camera_serial, expected.camera_serial);
            assert_eq!(actual.visible_region, expected.visible_region);
            assert_eq!(actual.root_transform, expected.root_transform);
            assert_eq!(actual.target_size, expected.target_size);
            assert_eq!(actual.target_origin, expected.target_origin);
            assert_eq!(actual.stats.generation, expected.stats.generation);
            assert_eq!(
                actual.stats.selected_stroke_segments,
                expected.stats.selected_stroke_segments
            );
        };
        assert!(receipt.record_execution(GpuDirectRenderOutcome::Encoded, Some(snapshot)));
        assert_eq!(receipt.outcome(), GpuDirectRenderOutcome::Encoded);
        assert_snapshot(snapshot);
        snapshot.visible_region.width = 16;
        snapshot.target_size[0] = 16;
        snapshot.stats.selected_stroke_segments = 2;
        assert!(!receipt.record_execution(GpuDirectRenderOutcome::Encoded, Some(snapshot)));
        assert_snapshot(snapshot);
        for outcome in [
            GpuDirectRenderOutcome::Failed,
            GpuDirectRenderOutcome::Superseded,
        ] {
            assert!(!receipt.record_execution(outcome, None));
            assert!(receipt.render_snapshot().is_none());
            assert_eq!(receipt.outcome(), GpuDirectRenderOutcome::Encoded);
        }
    }

    #[test]
    fn stale_direct_generation_is_superseded_not_failed() {
        assert_eq!(direct_render_encoded(Ok(())).unwrap(), Some(()));
        assert_eq!(
            direct_render_encoded::<()>(Err(RenderError::StaleRenderGeneration {
                requested: 1,
                latest: 2,
            }))
            .unwrap(),
            None
        );
        assert!(
            direct_render_encoded::<()>(Err(RenderError::InvalidRequest("bad".into()))).is_err()
        );
    }

    #[test]
    fn direct_render_region_tracks_window_clipping_in_page_space() {
        let visible = GpuPageRegion {
            x: 100,
            y: 200,
            width: 400,
            height: 300,
        };
        assert_eq!(
            clipped_direct_region(
                [-10.0, 20.0, 400.0, 300.0],
                [25.0, 40.0, 200.0, 150.0],
                [1_000, 800],
                visible,
                [2_000, 2_000],
            ),
            Some((
                [25, 40],
                GpuPageRegion {
                    x: 135,
                    y: 220,
                    width: 200,
                    height: 150,
                }
            ))
        );
    }

    #[test]
    fn full_render_deferral_is_opt_in() {
        assert!(!GpuPageRenderOptions::default().defer_full_render);
        assert!(
            GpuPageRenderOptions {
                defer_full_render: true,
            }
            .defer_full_render
        );
    }

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
        let (first, first_hit) = document
            .compile_page_with_cache_status(0, &cancellation)
            .expect("compile page");
        let (second, second_hit) = document
            .compile_page_with_cache_status(0, &cancellation)
            .expect("reuse page");
        assert!(Arc::ptr_eq(&first, &second));
        assert!(!first_hit);
        assert!(second_hit);
        assert_eq!(
            document.compiled_cache_bytes(),
            first.estimated_retained_bytes()
        );
        assert_eq!(document.compiled_cache_byte_limit(), 512 * 1024 * 1024);
    }

    #[test]
    fn encrypted_documents_never_create_a_compiled_disk_cache() {
        use base64::Engine as _;

        let encoded = include_str!("../../pdf/tests/fixtures/dummy-encrypted.pdf.b64")
            .split_whitespace()
            .collect::<String>();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("fixture");
        let snapshot = DocumentSnapshot::from_pdf_bytes("encrypted.pdf", bytes)
            .with_password(glyphst_pdf::DocumentPassword::new("123456"));
        let cache_root = std::env::temp_dir().join(format!(
            "glyphst-encrypted-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let document = GpuPdfDocument::open_with_compiled_cache(&snapshot, &cache_root)
            .expect("open encrypted PDF");
        assert!(document.is_encrypted());
        assert!(document.inner.compiled_cache_root.is_none());
        assert!(
            document
                .compiled_page_cache_path(PageIndex::new(0))
                .is_none()
        );
        let compiled = document
            .compile_page(0, &CancellationToken::new())
            .expect("compile encrypted page");
        document
            .persist_compiled_page_cache(&compiled)
            .expect("skip encrypted cache write");
        assert!(!cache_root.exists());
    }

    #[test]
    fn compiled_cache_enforces_bytes_lru_and_oversized_admission() {
        let snapshot =
            DocumentSnapshot::from_bytes("cache.pdf", DocumentFormat::Pdf, multipage_pdf(4));
        let document = GpuPdfDocument::open(&snapshot).expect("open PDF");
        let pages: Vec<_> = (0..4)
            .map(|index| {
                document
                    .compile_page(index, &CancellationToken::new())
                    .expect("compile page")
            })
            .collect();
        let mut cache = CompiledPageCache::new(100);
        cache.insert(PageIndex::new(0), Arc::clone(&pages[0]), 60);
        cache.insert(PageIndex::new(1), Arc::clone(&pages[1]), 40);
        assert_eq!(cache.bytes, 100);
        let oversized = cache.insert(PageIndex::new(2), Arc::clone(&pages[2]), 101);
        assert!(Arc::ptr_eq(&oversized, &pages[2]));
        assert_eq!(cache.entries.len(), 2);
        assert_eq!(cache.bytes, 100);
        assert!(cache.get(PageIndex::new(2)).is_none());

        cache.get(PageIndex::new(0)).expect("refresh first page");
        cache.insert(PageIndex::new(2), Arc::clone(&pages[2]), 30);
        assert!(cache.get(PageIndex::new(1)).is_none());
        assert_eq!(cache.bytes, 90);
        let reused = cache.insert(PageIndex::new(2), Arc::clone(&pages[3]), 99);
        assert!(Arc::ptr_eq(&reused, &pages[2]));
        assert_eq!(cache.bytes, 90);

        cache.insert(PageIndex::new(3), Arc::clone(&pages[3]), 95);
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.bytes, 95);
        assert!(cache.get(PageIndex::new(0)).is_none());
        assert!(cache.get(PageIndex::new(2)).is_none());

        let mut disabled = CompiledPageCache::new(0);
        disabled.insert(PageIndex::new(0), Arc::clone(&pages[0]), 1);
        assert!(disabled.entries.is_empty());
        let mut saturated = CompiledPageCache::new(u64::MAX);
        saturated.insert(PageIndex::new(0), Arc::clone(&pages[0]), u64::MAX);
        saturated.insert(PageIndex::new(1), Arc::clone(&pages[1]), 1);
        assert_eq!(saturated.bytes, 1);
        assert_eq!(saturated.entries.len(), 1);
    }

    #[test]
    fn compiled_page_disk_cache_reopens_only_validated_source_data() {
        let nonce = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let cache_root = std::env::temp_dir().join(format!(
            "glyphst-pdf-compiled-cache-{}-{nonce}",
            std::process::id()
        ));
        let snapshot =
            DocumentSnapshot::from_bytes("cached.pdf", DocumentFormat::Pdf, minimal_pdf());
        let first = GpuPdfDocument::open_with_compiled_cache(&snapshot, &cache_root)
            .expect("open first document");
        let (compiled, cache_hit) = first
            .compile_page_with_cache_status(0, &CancellationToken::new())
            .expect("compile first page");
        assert!(!cache_hit);
        first
            .persist_compiled_page_cache(&compiled)
            .expect("persist compiled page");

        let second = GpuPdfDocument::open_with_compiled_cache(&snapshot, &cache_root)
            .expect("reopen document");
        let (restored, cache_hit) = second
            .compile_page_with_cache_status(0, &CancellationToken::new())
            .expect("restore compiled page");
        assert!(cache_hit);
        assert_eq!(
            restored.scene().fingerprint(),
            compiled.scene().fingerprint()
        );

        let cache_path = first
            .compiled_page_cache_path(PageIndex::new(0))
            .expect("compiled cache path");
        fs::write(&cache_path, b"corrupt-zstd").expect("corrupt cache");
        let third = GpuPdfDocument::open_with_compiled_cache(&snapshot, &cache_root)
            .expect("reopen corrupt cache");
        let (_, cache_hit) = third
            .compile_page_with_cache_status(0, &CancellationToken::new())
            .expect("replace corrupt cache");
        assert!(!cache_hit);
        assert!(!cache_path.exists());

        fs::remove_dir_all(cache_root).expect("remove test cache");
    }

    #[test]
    fn native_cpu_page_export_honors_reader_rotation_without_gpu_readback() {
        let snapshot =
            DocumentSnapshot::from_bytes("native-export.pdf", DocumentFormat::Pdf, minimal_pdf());
        let document = GpuPdfDocument::open(&snapshot).expect("open native document");
        let image = document
            .render_page_image(
                0,
                DocumentRotation::Degrees90,
                2.0,
                &CancellationToken::new(),
            )
            .expect("render native page export");

        assert_eq!([image.width, image.height], [400, 200]);
        assert_eq!(image.rgba.len(), 400 * 200 * 4);
        assert!(image.rgba.chunks_exact(4).all(|pixel| pixel == [255; 4]));
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
    fn layer_hidden_links_keep_mapping_bounds_without_hits_or_actions() {
        for hidden in [false, true] {
            let bytes = String::from_utf8(link_pdf("/OC 6 0 R").to_vec())
                .expect("ASCII fixture")
                .replace("/Pages 2 0 R", &format!("/Pages 2 0 R/OCProperties<</OCGs[6 0 R]/D<</BaseState/{}>>>>", if hidden { "OFF" } else { "ON" }))
                .replace("/Annots[5 0 R]", "/Annots[5 0 R 7 0 R]")
                .replace("trailer", "6 0 obj <</Type/OCG/Name(Links)>> endobj\n7 0 obj <</Type/Annot/Subtype/Link/Rect[60 10 90 30]/Border[0 0 0]/H/I/A<</S/URI/URI(https://example.com/always)>>>> endobj\ntrailer");
            let snapshot = DocumentSnapshot::from_bytes(
                "annotation-layers.pdf",
                DocumentFormat::Pdf,
                Arc::<[u8]>::from(bytes.into_bytes()),
            );
            let document = GpuPdfDocument::open(&snapshot).expect("open annotation layers");
            let page = document
                .compile_page(0, &CancellationToken::new())
                .expect("compile layers");
            assert!(page.support().is_supported(), "{:#?}", page.support());
            let interactions = compiled_link_interactions(&page).expect("map links");
            assert_eq!(interactions.len(), 2);
            assert_eq!(interactions[0].annotation_bounds, [10.0, 70.0, 50.0, 90.0]);
            assert_eq!(interactions[0].contains_point(20.0, 80.0), !hidden);
            assert_eq!(interactions[0].action.is_inert(), hidden);
            assert_eq!(
                interactions[0].highlight_mode,
                if hidden {
                    GpuLinkHighlightMode::None
                } else {
                    GpuLinkHighlightMode::Invert
                }
            );
            assert_eq!(interactions[0].regions.is_empty(), hidden);
            assert!(matches!(page.links()[0].action, EngineAction::Uri(_)));
            assert!(interactions[1].contains_point(70.0, 80.0));
            assert!(!interactions[1].action.is_inert());
            let mupdf = snapshot.open().expect("open MuPDF mapping reference");
            let mupdf_page = mupdf.inner().load_page(0).expect("MuPDF page");
            let bounds = mupdf_page
                .links()
                .expect("MuPDF links")
                .map(|link| {
                    [
                        f64::from(link.bounds.x0),
                        f64::from(link.bounds.y0),
                        f64::from(link.bounds.x1),
                        f64::from(link.bounds.y1),
                    ]
                })
                .collect::<Vec<_>>();
            assert_eq!(bounds.len(), interactions.len());
            assert!(
                interactions
                    .iter()
                    .all(|interaction| bounds.contains(&interaction.annotation_bounds))
            );
        }
    }

    #[test]
    fn malformed_null_link_destination_stays_native_without_a_dead_hit_target() {
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
        let warnings = compiled_page_warnings(&page);
        assert!(warnings.iter().any(|diagnostic| {
            diagnostic.feature == FeatureId::Links
                && diagnostic.code == "null-link-destination-noop"
                && diagnostic.message.contains("null")
        }));
        let interactions = compiled_link_interactions(&page).expect("compile interactions");
        assert_eq!(interactions.len(), 1);
        assert!(interactions[0].action.is_inert());
    }

    #[test]
    fn malformed_empty_link_destination_stays_native_without_a_dead_hit_target() {
        let snapshot = DocumentSnapshot::from_bytes(
            "empty-link-destination.pdf",
            DocumentFormat::Pdf,
            link_pdf_with_action("/Dest[]"),
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
        let warnings = compiled_page_warnings(&page);
        assert!(warnings.iter().any(|diagnostic| {
            diagnostic.feature == FeatureId::Links
                && diagnostic.code == "invalid-link-destination-noop"
                && diagnostic.message.contains("destination-page-invalid")
        }));
        let interactions = compiled_link_interactions(&page).expect("compile interactions");
        assert_eq!(interactions.len(), 1);
        assert!(interactions[0].action.is_inert());
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
    fn only_all_noop_action_chains_are_inert() {
        let noop = GpuLinkAction::NoOp {
            code: Arc::from("invalid-link-destination-noop"),
            message: Arc::from("no destination"),
        };
        assert!(noop.is_inert());
        assert!(GpuLinkAction::Sequence(Arc::from([noop.clone()])).is_inert());
        assert!(
            !GpuLinkAction::Sequence(Arc::from([
                noop,
                GpuLinkAction::Named(GpuNamedAction::NextPage),
            ]))
            .is_inert()
        );
    }

    #[test]
    fn application_boundary_types_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<GpuPdfDocument>();
        assert_send_sync::<PreparedGpuPage>();
        assert_send_sync::<GpuPdfRenderer>();
        assert_send_sync::<GpuPagePresentation>();
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
    fn visible_regions_are_clamped_and_expanded_to_stable_tile_boundaries() {
        let request = GpuPaintRequest {
            page_size: [1_000, 2_000],
            visible_region: GpuPageRegion {
                x: 200,
                y: 500,
                width: 300,
                height: 400,
            },
            target_capacity: None,
            pan_cache_axes: [true; 2],
            full_page_pan_cache_axes: [false; 2],
            camera_target_capacity: None,
            camera_serial: 0,
            device_origin_phase: [0.0; 2],
            rotation: DocumentRotation::Degrees0,
            background: [1.0; 4],
            settled: false,
            generation: 1,
        };
        assert_eq!(
            request.surface_region().unwrap(),
            GpuPageRegion {
                x: 0,
                y: 256,
                width: 640,
                height: 896,
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
                x: 768,
                y: 1_792,
                width: 232,
                height: 208,
            }
        );

        let shifted = GpuPaintRequest {
            visible_region: GpuPageRegion {
                x: 201,
                y: 501,
                width: 300,
                height: 400,
            },
            ..request
        };
        assert_eq!(
            shifted.surface_region().unwrap(),
            request.surface_region().unwrap()
        );

        let large = GpuPaintRequest {
            page_size: [4_000, 4_000],
            visible_region: GpuPageRegion {
                x: 1_000,
                y: 1_000,
                width: 1_600,
                height: 1_000,
            },
            ..request
        };
        assert_eq!(
            large.surface_region().unwrap(),
            GpuPageRegion {
                x: 384,
                y: 640,
                width: 2_816,
                height: 1_792,
            },
            "large viewports retain a 1.7x exact-density pan cache before tile alignment"
        );

        let full_vertical = GpuPaintRequest {
            full_page_pan_cache_axes: [false, true],
            ..large
        };
        assert_eq!(
            full_vertical.surface_region().unwrap(),
            GpuPageRegion {
                x: 384,
                y: 0,
                width: 2_816,
                height: 4_000,
            },
            "a selected full page axis keeps the directional cross-axis cache",
        );
    }

    #[test]
    fn stable_capacity_is_used_only_for_the_normal_overscanned_target() {
        let request = GpuPaintRequest {
            page_size: [4_000, 4_000],
            visible_region: GpuPageRegion {
                x: 1_000,
                y: 1_000,
                width: 800,
                height: 600,
            },
            target_capacity: Some([2_048, 1_536]),
            pan_cache_axes: [true; 2],
            full_page_pan_cache_axes: [false; 2],
            camera_target_capacity: Some([1_536, 1_024]),
            camera_serial: 7,
            device_origin_phase: [0.25, -0.375],
            rotation: DocumentRotation::Degrees0,
            background: [1.0; 4],
            settled: false,
            generation: 1,
        };
        let overscanned = request
            .surface_region_for(SurfaceRegionMode::Overscanned)
            .unwrap();
        assert_eq!(
            request
                .target_size_for(overscanned, SurfaceRegionMode::Overscanned)
                .unwrap(),
            [2_048, 1_536]
        );

        let tile_aligned = request
            .surface_region_for(SurfaceRegionMode::TileAligned)
            .unwrap();
        assert_eq!(
            request
                .target_size_for(tile_aligned, SurfaceRegionMode::TileAligned)
                .unwrap(),
            tile_aligned.size(),
            "budget recovery must be able to discard unused capacity"
        );
        assert_eq!(
            request.preallocated_camera_target_size(8_192).unwrap(),
            Some([1_536, 1_024])
        );

        let too_small = GpuPaintRequest {
            camera_target_capacity: Some([512, 512]),
            ..request
        };
        assert_eq!(
            too_small.preallocated_camera_target_size(8_192).unwrap(),
            Some([800, 600]),
            "preallocation must cover the effective visible target"
        );
        for invalid in [[0, 1_024], [1_536, 0], [8_193, 1_024]] {
            let invalid = GpuPaintRequest {
                camera_target_capacity: Some(invalid),
                ..request
            };
            assert!(invalid.preallocated_camera_target_size(8_192).is_err());
        }
    }

    #[test]
    fn camera_surface_translation_applies_fractional_device_phase() {
        let request = GpuPaintRequest {
            page_size: [1_000, 2_000],
            visible_region: GpuPageRegion {
                x: 200,
                y: 500,
                width: 300,
                height: 400,
            },
            target_capacity: None,
            pan_cache_axes: [true; 2],
            full_page_pan_cache_axes: [false; 2],
            camera_target_capacity: None,
            camera_serial: 42,
            device_origin_phase: [0.25, -0.375],
            rotation: DocumentRotation::Degrees0,
            background: [1.0; 4],
            settled: true,
            generation: 1,
        };
        let region = request.surface_region().expect("surface region");
        let transform = camera_surface_translation(region, request).expect("camera translation");
        assert_eq!(
            transform.coefficients(),
            [
                1.0,
                0.0,
                0.0,
                1.0,
                -f64::from(region.x) + 0.25,
                -f64::from(region.y) - 0.375
            ]
        );

        for phase in [[f32::NAN, 0.0], [0.0, f32::INFINITY], [0.500_1, 0.0]] {
            assert!(
                camera_surface_translation(
                    region,
                    GpuPaintRequest {
                        device_origin_phase: phase,
                        ..request
                    },
                )
                .is_err()
            );
        }
    }

    #[test]
    fn presentation_ring_never_selects_current_or_retained_source() {
        let size = [640, 896];
        assert_eq!(
            presentation_surface_slot(&[], None, &[], size, false),
            SurfaceSlot::Insert
        );
        assert_eq!(
            presentation_surface_slot(&[size], Some(0), &[Some(0)], size, false),
            SurfaceSlot::Insert
        );
        assert_eq!(
            presentation_surface_slot(&[size, size], Some(1), &[Some(0)], size, false),
            SurfaceSlot::Insert
        );
        assert_eq!(
            presentation_surface_slot(&[size, size, size], Some(1), &[Some(0)], size, false),
            SurfaceSlot::Existing(2)
        );
        assert_eq!(
            presentation_surface_slot(&[size, size, size], Some(2), &[Some(0)], size, false),
            SurfaceSlot::Existing(1)
        );
        assert_eq!(
            presentation_surface_slot(
                &[size, size, size, size, size, size, size],
                Some(2),
                &[Some(0)],
                [768, 896],
                false,
            ),
            SurfaceSlot::Replace(3)
        );
        assert_eq!(
            presentation_surface_slot(&[size, size], Some(1), &[Some(0)], size, true),
            SurfaceSlot::Existing(1),
            "direct redraw may overwrite the current matching surface"
        );
        assert_eq!(
            presentation_surface_slot(&[size, size], Some(1), &[Some(0), Some(1)], size, true),
            SurfaceSlot::Insert,
            "direct redraw must not overwrite the coherent presented surface"
        );
    }

    #[test]
    fn working_render_identity_ignores_priority_only_request_fields() {
        let rect = |x0, y0, x1, y1| {
            glyphst_pdf_engine::Rect::from_corners(
                glyphst_pdf_engine::Point::new(x0, y0).unwrap(),
                glyphst_pdf_engine::Point::new(x1, y1).unwrap(),
            )
            .unwrap()
        };
        let bounds = rect(0.0, 0.0, 100.0, 200.0);
        let request = RenderRequest {
            root_transform: Transform::IDENTITY,
            viewport: bounds,
            target_size: [100, 200],
            background: [1.0; 4],
            settled: true,
            generation: 1,
        };
        assert!(same_render_content(
            request,
            RenderRequest {
                viewport: rect(20.0, 30.0, 60.0, 80.0),
                generation: 2,
                ..request
            }
        ));
        assert!(!same_render_content(
            request,
            RenderRequest {
                settled: false,
                ..request
            }
        ));
        assert!(!same_render_content(
            request,
            RenderRequest {
                target_size: [101, 200],
                ..request
            }
        ));
    }

    #[test]
    fn presentation_region_tightens_after_a_render_target_limit() {
        let request = GpuPaintRequest {
            page_size: [4_000, 4_000],
            visible_region: GpuPageRegion {
                x: 1_000,
                y: 1_000,
                width: 1_600,
                height: 1_000,
            },
            target_capacity: None,
            pan_cache_axes: [true; 2],
            full_page_pan_cache_axes: [false; 2],
            camera_target_capacity: None,
            camera_serial: 0,
            device_origin_phase: [0.0; 2],
            rotation: DocumentRotation::Degrees0,
            background: [1.0; 4],
            settled: false,
            generation: 1,
        };
        let mut presentation = GpuPagePresentation::default();
        assert_eq!(
            presentation.surface_region(request).unwrap(),
            GpuPageRegion {
                x: 384,
                y: 640,
                width: 2_816,
                height: 1_792,
            }
        );

        presentation.region_mode = SurfaceRegionMode::TileAligned;
        assert_eq!(
            presentation.surface_region(request).unwrap(),
            GpuPageRegion {
                x: 896,
                y: 896,
                width: 1_792,
                height: 1_152,
            }
        );

        presentation.region_mode = SurfaceRegionMode::Visible;
        assert_eq!(
            presentation.surface_region(request).unwrap(),
            request.visible_region
        );
        assert!(render_target_limit(&RenderError::Limit {
            resource: "transient target bytes",
            requested: 513,
            limit: 512,
        }));
        assert!(!render_target_limit(&RenderError::Limit {
            resource: "render dimension",
            requested: 513,
            limit: 512,
        }));
    }
}
