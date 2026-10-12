//! Page capture for GPU vector rendering.
//!
//! A MuPDF display list is replayed through a [`NativeDevice`] that records
//! fills, strokes and glyph outlines in page space, independent of zoom.
//! Rectangular clips become scissor rectangles and other path clips become
//! clip items the renderer applies with a stencil buffer. Operations a vector
//! renderer cannot reproduce exactly (shadings, images, soft masks,
//! non-normal blending, patterns, text and stroke clips) are kept
//! as ranges of display-list operations; [`rasterize_ops`] replays only those
//! ranges through MuPDF's own rasterizer, so paint order is preserved and no
//! page falls back as a whole.

use std::{cell::RefCell, num::NonZero, ops::Range, rc::Rc, sync::Arc};

use anyhow::{Context as _, Result};
use mupdf::{
    BlendMode, ColorParams, Colorspace, Device, DisplayList, Function, Image,
    LineCap as MupdfLineCap, Matrix, NativeDevice, Path, PathWalker, Pixmap, Rect, Shade,
    StrokeState, Text,
};

/// Axis-aligned rectangle in page space: `[x0, y0, x1, y1]`.
pub type PageRect = [f32; 4];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathVerb {
    MoveTo,
    LineTo,
    /// Consumes three points: two control points and the end point.
    CubicTo,
    Close,
}

/// A path whose points are already transformed into page space.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PagePath {
    pub verbs: Vec<PathVerb>,
    pub points: Vec<[f32; 2]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StrokeStyle {
    /// Line width in page units; zero is a hairline (one device pixel).
    pub width: f32,
    pub start_cap: LineCap,
    pub end_cap: LineCap,
    pub dash_cap: LineCap,
    /// Dash lengths and phase in page units; empty for solid lines.
    pub dashes: Vec<f32>,
    pub dash_phase: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum VectorItem {
    Fill {
        path: PagePath,
        even_odd: bool,
        color: [u8; 4],
    },
    Stroke {
        path: PagePath,
        style: StrokeStyle,
        color: [u8; 4],
    },
    /// A run of display-list operations rasterized by MuPDF, with the page
    /// area those operations can paint.
    Raster {
        ops: Range<u32>,
        bounds: PageRect,
    },
    /// An image at its own resolution. `transform` maps the unit square
    /// (`u`, `v` in 0..=1, pixel row 0 at `v` = 0) to page space:
    /// `x = a·u + c·v + e`, `y = b·u + d·v + f`.
    Image {
        rgba: ImagePixels,
        size: [u32; 2],
        transform: [f32; 6],
        /// Whether the PDF asks for smooth scaling when the image is
        /// enlarged; otherwise pixels stay square, as MuPDF draws them.
        interpolate: bool,
    },
    /// Restricts following items to `path` until the matching [`ClipPop`].
    ///
    /// [`ClipPop`]: VectorItem::ClipPop
    ClipPush {
        path: PagePath,
        even_odd: bool,
    },
    ClipPop,
}

/// Largest image side kept as a texture; larger images are rasterized.
pub const MAX_IMAGE_DIMENSION: u32 = 8192;

/// Premultiplied RGBA8 rows of an image, shared rather than copied.
#[derive(Clone, PartialEq)]
pub struct ImagePixels(pub Arc<[u8]>);

impl std::fmt::Debug for ImagePixels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ImagePixels({} bytes)", self.0.len())
    }
}

/// Converts an image to premultiplied RGBA with `alpha` applied, when that
/// is exact: device RGB or gray without a soft mask. Other images are left
/// to MuPDF's rasterizer.
fn image_rgba(image: &Image, alpha: f32) -> Option<(ImagePixels, [u32; 2])> {
    if image.mask().is_some() {
        return None;
    }
    let pixmap = image.to_pixmap().ok()?;
    let (width, height) = (pixmap.width(), pixmap.height());
    if width == 0 || height == 0 || width.max(height) > MAX_IMAGE_DIMENSION {
        return None;
    }
    let colorspace = pixmap.color_space()?;
    let has_alpha = pixmap.alpha();
    let n = usize::from(pixmap.n());
    let colors = n - usize::from(has_alpha);
    let gray = colorspace.is_gray() && colors == 1;
    let rgb = colorspace.is_rgb() && colors == 3;
    if !(gray || rgb) {
        return None;
    }
    let stride = usize::try_from(pixmap.stride()).ok()?;
    let samples = pixmap.samples();
    let scale = (alpha.clamp(0.0, 1.0) * 255.0).round() as u16;
    let mul = |value: u8| ((u16::from(value) * scale + 127) / 255) as u8;
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for row in samples.chunks(stride).take(height as usize) {
        for pixel in row[..width as usize * n].chunks_exact(n) {
            // Pixmap samples with alpha are already premultiplied.
            let a = if has_alpha { pixel[n - 1] } else { 255 };
            let (r, g, b) = if gray {
                (pixel[0], pixel[0], pixel[0])
            } else {
                (pixel[0], pixel[1], pixel[2])
            };
            rgba.extend_from_slice(&[mul(r), mul(g), mul(b), mul(a)]);
        }
    }
    Some((ImagePixels(rgba.into()), [width, height]))
}

/// An image mask painted in `color` (straight RGBA8): the mask's coverage
/// becomes alpha, premultiplied into the colour.
fn mask_rgba(image: &Image, color: [u8; 4]) -> Option<(ImagePixels, [u32; 2])> {
    let pixmap = image.to_pixmap().ok()?;
    let (width, height) = (pixmap.width(), pixmap.height());
    if width == 0 || height == 0 || width.max(height) > MAX_IMAGE_DIMENSION || pixmap.n() != 1 {
        return None;
    }
    let stride = usize::try_from(pixmap.stride()).ok()?;
    let premultiplied = |coverage: u8| {
        let alpha = u16::from(coverage) * u16::from(color[3]) / 255;
        let channel = |value: u8| ((u16::from(value) * alpha + 127) / 255) as u8;
        [
            channel(color[0]),
            channel(color[1]),
            channel(color[2]),
            alpha as u8,
        ]
    };
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for row in pixmap.samples().chunks(stride).take(height as usize) {
        for &coverage in &row[..width as usize] {
            rgba.extend_from_slice(&premultiplied(coverage));
        }
    }
    Some((ImagePixels(rgba.into()), [width, height]))
}

#[derive(Clone, Debug, PartialEq)]
pub struct VectorDraw {
    pub item: VectorItem,
    /// Intersection of the active rectangular clips, if any.
    pub clip: Option<PageRect>,
}

/// Why operations were routed to a raster layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RasterReason {
    Shading,
    Image,
    ImageMask,
    GlyphWithoutOutline,
    StrokedText,
    StrokeClip,
    TextClip,
    ImageMaskClip,
    SoftMask,
    BlendGroup,
    Pattern,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CaptureStats {
    pub operations: u32,
    pub fills: usize,
    pub strokes: usize,
    pub glyphs: usize,
    pub images: usize,
    pub path_points: usize,
    pub raster_layers: usize,
    /// Paint operations routed to raster layers, by reason.
    pub raster_ops: Vec<(RasterReason, usize)>,
}

impl CaptureStats {
    pub fn raster_op_count(&self) -> usize {
        self.raster_ops.iter().map(|(_, count)| count).sum()
    }

    fn count_raster(&mut self, reason: RasterReason) {
        match self.raster_ops.iter_mut().find(|(r, _)| *r == reason) {
            Some((_, count)) => *count += 1,
            None => self.raster_ops.push((reason, 1)),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VectorPage {
    pub bounds: PageRect,
    pub draws: Vec<VectorDraw>,
    pub stats: CaptureStats,
}

/// Records `display_list` (created with the identity matrix) in page space.
pub fn capture_display_list(display_list: &DisplayList) -> Result<VectorPage> {
    let bounds = display_list.bounds();
    let state = Rc::new(RefCell::new(Capture::new(bounds)));
    let device = Device::from_native(CaptureDevice(state.clone()))
        .context("failed to create vector capture device")?;
    display_list
        .run(&device, &Matrix::IDENTITY, Rect::INF)
        .context("failed to capture PDF page vectors")?;
    drop(device);
    let capture = state.replace(Capture::new(bounds));
    Ok(capture.finish())
}

/// Rasterizes the display-list operations in `ops` onto a transparent RGBA
/// pixmap of `width` x `height` pixels, with `ctm` mapping page space to it.
pub fn rasterize_ops(
    display_list: &DisplayList,
    ops: Range<u32>,
    ctm: &Matrix,
    width: u32,
    height: u32,
) -> Result<Pixmap> {
    let mut pixmap = Pixmap::new_with_w_h(
        &Colorspace::device_rgb(),
        i32::try_from(width).context("raster layer width overflow")?,
        i32::try_from(height).context("raster layer height overflow")?,
        true,
    )
    .context("failed to allocate raster layer")?;
    pixmap.clear().context("failed to clear raster layer")?;
    let target = Device::from_pixmap(&pixmap).context("failed to create raster layer device")?;
    let filter = Device::from_native(RangeFilter {
        target,
        ops,
        next_op: 0,
    })
    .context("failed to create raster layer filter")?;
    // An unbounded area: MuPDF skips operations outside a bounded one, which
    // would shift the numbering the ranges refer to. The pixmap clips.
    display_list
        .run(&filter, ctm, Rect::INF)
        .context("failed to rasterize PDF raster layer")?;
    drop(filter);
    Ok(pixmap)
}

/// A raster layer rasterized over its page bounds.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerImage {
    /// Page area the image covers; slightly larger than the requested
    /// bounds because the pixel size is rounded up.
    pub rect: PageRect,
    pub size: [u32; 2],
    /// Premultiplied RGBA8 rows.
    pub rgba: Vec<u8>,
}

/// Rasterizes the operations `ops`, which paint within `bounds`, at `scale`
/// device pixels per page point, keeping both sides within `max_dimension`
/// pixels. Returns `None` for empty bounds.
pub fn rasterize_layer(
    display_list: &DisplayList,
    ops: Range<u32>,
    bounds: PageRect,
    scale: f32,
    max_dimension: u32,
) -> Result<Option<LayerImage>> {
    let [x0, y0, x1, y1] = bounds;
    if x1 <= x0 || y1 <= y0 {
        return Ok(None);
    }
    let scale = scale
        .min(max_dimension as f32 / (x1 - x0))
        .min(max_dimension as f32 / (y1 - y0));
    let width = ((x1 - x0) * scale).ceil().max(1.0) as u32;
    let height = ((y1 - y0) * scale).ceil().max(1.0) as u32;
    let ctm = Matrix::new(scale, 0.0, 0.0, scale, -x0 * scale, -y0 * scale);
    let pixmap = rasterize_ops(display_list, ops, &ctm, width, height)?;
    Ok(Some(LayerImage {
        rect: [
            x0,
            y0,
            x0 + width as f32 / scale,
            y0 + height as f32 / scale,
        ],
        size: [width, height],
        rgba: pixmap.samples().to_vec(),
    }))
}

#[derive(Clone, Copy)]
struct ClipEntry {
    /// Exact rectangle for vector scissoring; `None` when not rectangular.
    rect: Option<PageRect>,
    /// Conservative bounds of the clipped area.
    bounds: PageRect,
    /// Whether this entry forced its contents into raster layers.
    forced: Option<RasterReason>,
    /// Whether this entry emitted a [`VectorItem::ClipPush`].
    stencil: bool,
}

struct Capture {
    bounds: PageRect,
    rgb: Colorspace,
    draws: Vec<VectorDraw>,
    stats: CaptureStats,
    next_op: u32,
    clips: Vec<ClipEntry>,
    groups: Vec<Option<RasterReason>>,
    tiles: u32,
    /// Index into `draws` of the raster run still accepting operations.
    open_raster: Option<usize>,
}

impl Capture {
    fn new(bounds: Rect) -> Self {
        Self {
            bounds: [bounds.x0, bounds.y0, bounds.x1, bounds.y1],
            rgb: Colorspace::device_rgb(),
            draws: Vec::new(),
            stats: CaptureStats::default(),
            next_op: 0,
            clips: Vec::new(),
            groups: Vec::new(),
            tiles: 0,
            open_raster: None,
        }
    }

    fn finish(mut self) -> VectorPage {
        self.stats.operations = self.next_op;
        self.stats.raster_layers = self
            .draws
            .iter()
            .filter(|draw| matches!(draw.item, VectorItem::Raster { .. }))
            .count();
        self.stats.raster_ops.sort();
        VectorPage {
            bounds: self.bounds,
            draws: self.draws,
            stats: self.stats,
        }
    }

    fn take_op(&mut self) -> u32 {
        let op = self.next_op;
        self.next_op += 1;
        op
    }

    /// The reason enclosing state forces paint into raster layers, if any.
    fn forced(&self) -> Option<RasterReason> {
        if self.tiles > 0 {
            return Some(RasterReason::Pattern);
        }
        self.clips
            .iter()
            .rev()
            .find_map(|clip| clip.forced)
            .or_else(|| self.groups.iter().rev().find_map(|group| *group))
    }

    fn clip_rect(&self) -> Option<PageRect> {
        self.clips
            .iter()
            .filter_map(|clip| clip.rect)
            .reduce(intersect)
    }

    /// Routes paint operation `op`, which can paint within `paint_bounds`
    /// (page space, before clipping), to the open raster layer.
    fn raster(&mut self, op: u32, reason: RasterReason, paint_bounds: Option<Rect>) {
        self.stats.count_raster(self.forced().unwrap_or(reason));
        let mut bounds = self.bounds;
        for clip in &self.clips {
            bounds = intersect(bounds, clip.bounds);
        }
        if let Some(paint) = paint_bounds.filter(|rect| rect.is_valid()) {
            bounds = intersect(bounds, [paint.x0, paint.y0, paint.x1, paint.y1]);
        }
        if bounds[2] <= bounds[0] || bounds[3] <= bounds[1] {
            bounds = [bounds[0], bounds[1], bounds[0], bounds[1]];
        }
        if let Some(index) = self.open_raster
            && let VectorItem::Raster { ops, bounds: layer } = &mut self.draws[index].item
        {
            ops.end = op + 1;
            *layer = union(*layer, bounds);
            return;
        }
        self.open_raster = Some(self.draws.len());
        self.draws.push(VectorDraw {
            item: VectorItem::Raster {
                ops: op..op + 1,
                bounds,
            },
            clip: None,
        });
    }

    fn push_vector(&mut self, item: VectorItem) {
        self.open_raster = None;
        let clip = self.clip_rect();
        self.draws.push(VectorDraw { item, clip });
    }

    fn color(
        &self,
        colorspace: &Colorspace,
        color: &[f32],
        alpha: f32,
        cp: ColorParams,
    ) -> [u8; 4] {
        let rgb = colorspace
            .convert_color(color, &self.rgb, None, cp)
            .unwrap_or_else(|_| vec![0.0; 3]);
        let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        [
            channel(rgb[0]),
            channel(rgb[1]),
            channel(rgb[2]),
            channel(alpha),
        ]
    }

    fn push_clip(&mut self, rect: Option<PageRect>, bounds: Rect, forced: Option<RasterReason>) {
        let bounds = if bounds.is_valid() {
            [bounds.x0, bounds.y0, bounds.x1, bounds.y1]
        } else {
            self.bounds
        };
        self.clips.push(ClipEntry {
            rect,
            bounds,
            forced,
            stencil: false,
        });
    }
}

struct CaptureDevice(Rc<RefCell<Capture>>);

impl NativeDevice for CaptureDevice {
    fn fill_path(
        &mut self,
        path: &Path,
        even_odd: bool,
        ctm: Matrix,
        color_space: &Colorspace,
        color: &[f32],
        alpha: f32,
        cp: ColorParams,
    ) {
        let mut capture = self.0.borrow_mut();
        let op = capture.take_op();
        if let Some(reason) = capture.forced() {
            let bounds = path.bounds(&StrokeState::default(), &ctm).ok();
            capture.raster(op, reason, bounds);
            return;
        }
        let page_path = page_path(path, &ctm);
        let color = capture.color(color_space, color, alpha, cp);
        capture.stats.fills += 1;
        capture.stats.path_points += page_path.points.len();
        capture.push_vector(VectorItem::Fill {
            path: page_path,
            even_odd,
            color,
        });
    }

    fn stroke_path(
        &mut self,
        path: &Path,
        stroke_state: &StrokeState,
        ctm: Matrix,
        color_space: &Colorspace,
        color: &[f32],
        alpha: f32,
        cp: ColorParams,
    ) {
        let mut capture = self.0.borrow_mut();
        let op = capture.take_op();
        if let Some(reason) = capture.forced() {
            let bounds = path.bounds(stroke_state, &ctm).ok();
            capture.raster(op, reason, bounds);
            return;
        }
        let page_path = page_path(path, &ctm);
        let color = capture.color(color_space, color, alpha, cp);
        capture.stats.strokes += 1;
        capture.stats.path_points += page_path.points.len();
        capture.push_vector(VectorItem::Stroke {
            path: page_path,
            style: stroke_style(stroke_state, &ctm),
            color,
        });
    }

    fn clip_path(&mut self, path: &Path, even_odd: bool, ctm: Matrix, scissor: Rect) {
        let mut capture = self.0.borrow_mut();
        capture.take_op();
        let page_path = page_path(path, &ctm);
        match rectangle(&page_path) {
            Some(rect) => capture.push_clip(Some(rect), scissor, None),
            // Inside a raster run MuPDF applies the clip itself.
            None if capture.forced().is_some() => capture.push_clip(None, scissor, None),
            None => {
                capture.push_clip(None, scissor, None);
                if let Some(clip) = capture.clips.last_mut() {
                    clip.stencil = true;
                }
                capture.stats.path_points += page_path.points.len();
                capture.push_vector(VectorItem::ClipPush {
                    path: page_path,
                    even_odd,
                });
            }
        }
    }

    fn clip_stroke_path(&mut self, _: &Path, _: &StrokeState, _: Matrix, scissor: Rect) {
        let mut capture = self.0.borrow_mut();
        capture.take_op();
        capture.push_clip(None, scissor, Some(RasterReason::StrokeClip));
    }

    fn fill_text(
        &mut self,
        text: &Text,
        ctm: Matrix,
        color_space: &Colorspace,
        color: &[f32],
        alpha: f32,
        cp: ColorParams,
    ) {
        let mut capture = self.0.borrow_mut();
        let op = capture.take_op();
        let bounds = || text.bounds(&StrokeState::default(), &ctm).ok();
        if let Some(reason) = capture.forced() {
            capture.raster(op, reason, bounds());
            return;
        }
        let mut outlines = Vec::new();
        for span in text.spans() {
            let font = span.font();
            let trm = span.trm();
            for item in span.items() {
                if item.gid() < 0 {
                    continue;
                }
                let mut glyph = trm.clone();
                glyph.e = item.x();
                glyph.f = item.y();
                glyph.concat(ctm.clone());
                match font.outline_glyph_with_ctm(item.gid(), &glyph) {
                    Ok(Some(outline)) => outlines.push(page_path(&outline, &Matrix::IDENTITY)),
                    Ok(None) | Err(_) => {
                        capture.raster(op, RasterReason::GlyphWithoutOutline, bounds());
                        return;
                    }
                }
            }
        }
        let color = capture.color(color_space, color, alpha, cp);
        for outline in outlines {
            capture.stats.glyphs += 1;
            capture.stats.path_points += outline.points.len();
            capture.push_vector(VectorItem::Fill {
                path: outline,
                even_odd: false,
                color,
            });
        }
    }

    fn stroke_text(
        &mut self,
        text: &Text,
        stroke_state: &StrokeState,
        ctm: Matrix,
        _: &Colorspace,
        _: &[f32],
        _: f32,
        _: ColorParams,
    ) {
        let mut capture = self.0.borrow_mut();
        let op = capture.take_op();
        let bounds = text.bounds(stroke_state, &ctm).ok();
        capture.raster(op, RasterReason::StrokedText, bounds);
    }

    fn clip_text(&mut self, _: &Text, _: Matrix, scissor: Rect) {
        let mut capture = self.0.borrow_mut();
        capture.take_op();
        capture.push_clip(None, scissor, Some(RasterReason::TextClip));
    }

    fn clip_stroke_text(&mut self, _: &Text, _: &StrokeState, _: Matrix, scissor: Rect) {
        let mut capture = self.0.borrow_mut();
        capture.take_op();
        capture.push_clip(None, scissor, Some(RasterReason::TextClip));
    }

    fn ignore_text(&mut self, _: &Text, _: Matrix) {
        self.0.borrow_mut().take_op();
    }

    fn fill_shade(&mut self, _: &Shade, _: Matrix, _: f32, _: ColorParams) {
        // Shadings are bounded only by the active clips.
        let mut capture = self.0.borrow_mut();
        let op = capture.take_op();
        capture.raster(op, RasterReason::Shading, None);
    }

    fn fill_image(&mut self, image: &Image, ctm: Matrix, alpha: f32, _: ColorParams) {
        let mut capture = self.0.borrow_mut();
        let op = capture.take_op();
        let converted = capture
            .forced()
            .is_none()
            .then(|| image_rgba(image, alpha))
            .flatten();
        match converted {
            Some((rgba, size)) => {
                capture.stats.images += 1;
                capture.push_vector(VectorItem::Image {
                    rgba,
                    size,
                    transform: [ctm.a, ctm.b, ctm.c, ctm.d, ctm.e, ctm.f],
                    interpolate: image.interpolate(),
                });
            }
            None => capture.raster(op, RasterReason::Image, Some(unit_square(&ctm))),
        }
    }

    fn fill_image_mask(
        &mut self,
        image: &Image,
        ctm: Matrix,
        color_space: &Colorspace,
        color: &[f32],
        alpha: f32,
        cp: ColorParams,
    ) {
        let mut capture = self.0.borrow_mut();
        let op = capture.take_op();
        let color = capture.color(color_space, color, alpha, cp);
        let converted = capture
            .forced()
            .is_none()
            .then(|| mask_rgba(image, color))
            .flatten();
        match converted {
            Some((rgba, size)) => {
                capture.stats.images += 1;
                capture.push_vector(VectorItem::Image {
                    rgba,
                    size,
                    transform: [ctm.a, ctm.b, ctm.c, ctm.d, ctm.e, ctm.f],
                    interpolate: image.interpolate(),
                });
            }
            None => capture.raster(op, RasterReason::ImageMask, Some(unit_square(&ctm))),
        }
    }

    fn clip_image_mask(&mut self, _: &Image, _: Matrix, scissor: Rect) {
        let mut capture = self.0.borrow_mut();
        capture.take_op();
        capture.push_clip(None, scissor, Some(RasterReason::ImageMaskClip));
    }

    fn pop_clip(&mut self) {
        let mut capture = self.0.borrow_mut();
        capture.take_op();
        if capture.clips.pop().is_some_and(|clip| clip.stencil) {
            capture.push_vector(VectorItem::ClipPop);
        }
    }

    fn begin_mask(&mut self, area: Rect, _: bool, _: &Colorspace, _: &[f32], _: ColorParams) {
        // The mask applies until the matching pop_clip.
        let mut capture = self.0.borrow_mut();
        capture.take_op();
        capture.push_clip(None, area, Some(RasterReason::SoftMask));
    }

    fn end_mask(&mut self, _: &Function) {
        self.0.borrow_mut().take_op();
    }

    fn begin_group(
        &mut self,
        _: Rect,
        _: &Colorspace,
        _: bool,
        knockout: bool,
        blendmode: BlendMode,
        alpha: f32,
    ) {
        let mut capture = self.0.borrow_mut();
        capture.take_op();
        let transparent = blendmode == BlendMode::Normal && !knockout && alpha >= 1.0;
        capture
            .groups
            .push((!transparent).then_some(RasterReason::BlendGroup));
    }

    fn end_group(&mut self) {
        let mut capture = self.0.borrow_mut();
        capture.take_op();
        capture.groups.pop();
    }

    fn begin_tile(
        &mut self,
        _: Rect,
        _: Rect,
        _: f32,
        _: f32,
        _: Matrix,
        _: Option<NonZero<i32>>,
        _: Option<NonZero<i32>>,
    ) -> Option<NonZero<i32>> {
        let mut capture = self.0.borrow_mut();
        capture.take_op();
        capture.tiles += 1;
        None
    }

    fn end_tile(&mut self) {
        let mut capture = self.0.borrow_mut();
        capture.take_op();
        capture.tiles = capture.tiles.saturating_sub(1);
    }
}

/// Replays a display list into `target`, keeping every structural operation
/// (clips, groups, masks, tiles) but only the paint operations in `ops`.
/// Operation numbering must match [`CaptureDevice`].
struct RangeFilter {
    target: Device,
    ops: Range<u32>,
    next_op: u32,
}

impl RangeFilter {
    fn paint(&mut self) -> bool {
        let op = self.next_op;
        self.next_op += 1;
        self.ops.contains(&op)
    }

    fn structure(&mut self) {
        self.next_op += 1;
    }
}

// Errors from individual forwarded operations are dropped, as MuPDF's own
// draw device does when a single operation fails.
impl NativeDevice for RangeFilter {
    fn fill_path(
        &mut self,
        path: &Path,
        even_odd: bool,
        ctm: Matrix,
        color_space: &Colorspace,
        color: &[f32],
        alpha: f32,
        cp: ColorParams,
    ) {
        if self.paint() {
            let _ = self
                .target
                .fill_path(path, even_odd, &ctm, color_space, color, alpha, cp);
        }
    }

    fn stroke_path(
        &mut self,
        path: &Path,
        stroke_state: &StrokeState,
        ctm: Matrix,
        color_space: &Colorspace,
        color: &[f32],
        alpha: f32,
        cp: ColorParams,
    ) {
        if self.paint() {
            let _ =
                self.target
                    .stroke_path(path, stroke_state, &ctm, color_space, color, alpha, cp);
        }
    }

    fn clip_path(&mut self, path: &Path, even_odd: bool, ctm: Matrix, _: Rect) {
        self.structure();
        let _ = self.target.clip_path(path, even_odd, &ctm);
    }

    fn clip_stroke_path(&mut self, path: &Path, stroke_state: &StrokeState, ctm: Matrix, _: Rect) {
        self.structure();
        let _ = self.target.clip_stroke_path(path, stroke_state, &ctm);
    }

    fn fill_text(
        &mut self,
        text: &Text,
        ctm: Matrix,
        color_space: &Colorspace,
        color: &[f32],
        alpha: f32,
        cp: ColorParams,
    ) {
        if self.paint() {
            let _ = self
                .target
                .fill_text(text, &ctm, color_space, color, alpha, cp);
        }
    }

    fn stroke_text(
        &mut self,
        text: &Text,
        stroke_state: &StrokeState,
        ctm: Matrix,
        color_space: &Colorspace,
        color: &[f32],
        alpha: f32,
        cp: ColorParams,
    ) {
        if self.paint() {
            let _ =
                self.target
                    .stroke_text(text, stroke_state, &ctm, color_space, color, alpha, cp);
        }
    }

    fn clip_text(&mut self, text: &Text, ctm: Matrix, _: Rect) {
        self.structure();
        let _ = self.target.clip_text(text, &ctm);
    }

    fn clip_stroke_text(&mut self, text: &Text, stroke_state: &StrokeState, ctm: Matrix, _: Rect) {
        self.structure();
        let _ = self.target.clip_stroke_text(text, stroke_state, &ctm);
    }

    fn ignore_text(&mut self, _: &Text, _: Matrix) {
        self.structure();
    }

    fn fill_shade(&mut self, shade: &Shade, ctm: Matrix, alpha: f32, cp: ColorParams) {
        if self.paint() {
            let _ = self.target.fill_shade(shade, &ctm, alpha, cp);
        }
    }

    fn fill_image(&mut self, image: &Image, ctm: Matrix, alpha: f32, cp: ColorParams) {
        if self.paint() {
            let _ = self.target.fill_image(image, &ctm, alpha, cp);
        }
    }

    fn fill_image_mask(
        &mut self,
        image: &Image,
        ctm: Matrix,
        color_space: &Colorspace,
        color: &[f32],
        alpha: f32,
        cp: ColorParams,
    ) {
        if self.paint() {
            let _ = self
                .target
                .fill_image_mask(image, &ctm, color_space, color, alpha, cp);
        }
    }

    fn clip_image_mask(&mut self, image: &Image, ctm: Matrix, _: Rect) {
        self.structure();
        let _ = self.target.clip_image_mask(image, &ctm);
    }

    fn pop_clip(&mut self) {
        self.structure();
        let _ = self.target.pop_clip();
    }

    fn begin_mask(
        &mut self,
        area: Rect,
        luminosity: bool,
        color_space: &Colorspace,
        color: &[f32],
        cp: ColorParams,
    ) {
        self.structure();
        let _ = self
            .target
            .begin_mask(area, luminosity, color_space, color, cp);
    }

    fn end_mask(&mut self, function: &Function) {
        self.structure();
        let _ = self.target.end_mask(Some(function));
    }

    fn begin_group(
        &mut self,
        area: Rect,
        color_space: &Colorspace,
        isolated: bool,
        knockout: bool,
        blendmode: BlendMode,
        alpha: f32,
    ) {
        self.structure();
        let _ = self
            .target
            .begin_group(area, color_space, isolated, knockout, blendmode, alpha);
    }

    fn end_group(&mut self) {
        self.structure();
        let _ = self.target.end_group();
    }

    fn begin_tile(
        &mut self,
        area: Rect,
        view: Rect,
        x_step: f32,
        y_step: f32,
        ctm: Matrix,
        _: Option<NonZero<i32>>,
        _: Option<NonZero<i32>>,
    ) -> Option<NonZero<i32>> {
        self.structure();
        // Tile caching is disabled so contents replay with matching numbering.
        let _ = self
            .target
            .begin_tile(area, view, x_step, y_step, &ctm, None, None);
        None
    }

    fn end_tile(&mut self) {
        self.structure();
        let _ = self.target.end_tile();
    }
}

struct PageSpaceWalker<'a> {
    ctm: &'a Matrix,
    path: PagePath,
}

impl PageSpaceWalker<'_> {
    fn point(&mut self, x: f32, y: f32) {
        let (x, y) = self.ctm.transform_xy(x, y);
        self.path.points.push([x, y]);
    }
}

impl PathWalker for PageSpaceWalker<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.verbs.push(PathVerb::MoveTo);
        self.point(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.path.verbs.push(PathVerb::LineTo);
        self.point(x, y);
    }

    fn curve_to(&mut self, cx1: f32, cy1: f32, cx2: f32, cy2: f32, ex: f32, ey: f32) {
        self.path.verbs.push(PathVerb::CubicTo);
        self.point(cx1, cy1);
        self.point(cx2, cy2);
        self.point(ex, ey);
    }

    fn close(&mut self) {
        self.path.verbs.push(PathVerb::Close);
    }
}

fn page_path(path: &Path, ctm: &Matrix) -> PagePath {
    let mut walker = PageSpaceWalker {
        ctm,
        path: PagePath::default(),
    };
    // A walk only fails on allocation errors; keep what was recorded.
    let _ = path.walk(&mut walker);
    walker.path
}

/// Returns the rectangle when `path` is a single axis-aligned rectangle.
fn rectangle(path: &PagePath) -> Option<PageRect> {
    if path.verbs.contains(&PathVerb::CubicTo)
        || path
            .verbs
            .iter()
            .filter(|verb| **verb == PathVerb::MoveTo)
            .count()
            != 1
    {
        return None;
    }
    let mut points = path.points.clone();
    if points.len() == 5 && points[0] == points[4] {
        points.pop();
    }
    if points.len() != 4 {
        return None;
    }
    let axis_aligned = (0..4).all(|index| {
        let a = points[index];
        let b = points[(index + 1) % 4];
        (a[0] - b[0]).abs() < 1e-3 || (a[1] - b[1]).abs() < 1e-3
    });
    if !axis_aligned {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for [x, y] in points {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    Some([x0, y0, x1, y1])
}

fn unit_square(ctm: &Matrix) -> Rect {
    Rect::new(0.0, 0.0, 1.0, 1.0).transform(ctm)
}

fn intersect(a: PageRect, b: PageRect) -> PageRect {
    [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ]
}

fn union(a: PageRect, b: PageRect) -> PageRect {
    if a[2] <= a[0] || a[3] <= a[1] {
        return b;
    }
    if b[2] <= b[0] || b[3] <= b[1] {
        return a;
    }
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

fn stroke_style(state: &StrokeState, ctm: &Matrix) -> StrokeStyle {
    let scale = ctm.expansion();
    let cap = |cap: MupdfLineCap| match cap {
        MupdfLineCap::Round => LineCap::Round,
        MupdfLineCap::Square => LineCap::Square,
        // Triangle caps are rare; a butt cap is the closest analytic shape.
        MupdfLineCap::Butt | MupdfLineCap::Triangle => LineCap::Butt,
    };
    StrokeStyle {
        width: state.line_width() * scale,
        start_cap: cap(state.start_cap()),
        end_cap: cap(state.end_cap()),
        dash_cap: cap(state.dash_cap()),
        dashes: state.dashes().iter().map(|dash| dash * scale).collect(),
        dash_phase: state.dash_phase() * scale,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glyphst_core::DocumentLayout;
    use mupdf::{Matrix, Rect};

    use super::*;
    use crate::{DocumentRenderSession, DocumentSnapshot};

    fn capture(content: &str, resources: &str) -> (VectorPage, crate::RetainedDisplayList) {
        let snapshot = Arc::new(DocumentSnapshot::from_pdf_bytes(
            "vector.pdf",
            crate::test_pdf::single_page_pdf(content, resources, ""),
        ));
        let session = DocumentRenderSession::open(snapshot, DocumentLayout::Fixed).unwrap();
        let list = session
            .create_display_list(0, Rect::new(0.0, 0.0, 200.0, 100.0))
            .unwrap();
        (capture_display_list(&list).unwrap(), list)
    }

    fn kinds(page: &VectorPage) -> Vec<&'static str> {
        page.draws
            .iter()
            .map(|draw| match draw.item {
                VectorItem::Fill { .. } => "fill",
                VectorItem::Stroke { .. } => "stroke",
                VectorItem::Raster { .. } => "raster",
                VectorItem::Image { .. } => "image",
                VectorItem::ClipPush { .. } => "clip",
                VectorItem::ClipPop => "unclip",
            })
            .collect()
    }

    const SHADING: &str = "<< /Shading << /Sh0 << /ShadingType 2 /ColorSpace /DeviceRGB \
        /Coords [0 0 200 0] /Function << /FunctionType 2 /Domain [0 1] \
        /C0 [1 0 0] /C1 [0 0 1] /N 1 >> >> >> >>";

    #[test]
    fn fills_and_strokes_are_captured_in_page_space_and_order() {
        let (page, _) = capture(
            "1 0 0 rg 10 10 30 20 re f 0 0 1 RG 2 w 50 50 m 150 50 l S",
            "<< >>",
        );
        assert_eq!(kinds(&page), ["fill", "stroke"]);
        // PDF y-up user space maps to y-down page space.
        let VectorItem::Fill { path, color, .. } = &page.draws[0].item else {
            unreachable!()
        };
        assert_eq!(color, &[255, 0, 0, 255]);
        assert!(path.points.contains(&[10.0, 90.0]));
        let VectorItem::Stroke { style, path, .. } = &page.draws[1].item else {
            unreachable!()
        };
        assert_eq!(style.width, 2.0);
        assert_eq!(path.points, [[50.0, 50.0], [150.0, 50.0]]);
        assert_eq!(page.stats.raster_op_count(), 0);
    }

    #[test]
    fn text_becomes_glyph_outline_fills() {
        let (page, _) = capture(
            "BT /F1 24 Tf 10 40 Td (Hi) Tj ET",
            "<< /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>",
        );
        assert_eq!(page.stats.glyphs, 2);
        assert_eq!(kinds(&page), ["fill", "fill"]);
    }

    #[test]
    fn shading_becomes_an_ordered_raster_layer() {
        let (page, _) = capture("0 g 0 0 10 10 re f /Sh0 sh 1 g 20 20 10 10 re f", SHADING);
        assert_eq!(kinds(&page), ["fill", "raster", "fill"]);
        assert_eq!(page.stats.raster_ops, [(RasterReason::Shading, 1)]);
    }

    #[test]
    fn rectangular_clips_scissor_and_path_clips_become_clip_items() {
        let (page, _) = capture(
            "q 0 0 100 50 re W n 0 g 0 0 200 100 re f Q \
             q 0 0 m 100 0 l 50 80 l h W n 0 g 0 0 200 100 re f Q",
            "<< >>",
        );
        assert_eq!(kinds(&page), ["fill", "clip", "fill", "unclip"]);
        assert_eq!(page.draws[0].clip, Some([0.0, 50.0, 100.0, 100.0]));
        assert_eq!(page.draws[2].clip, None);
        assert_eq!(page.stats.raster_op_count(), 0);
    }

    #[test]
    fn raster_layer_contains_only_its_operations() {
        let (page, list) = capture("0 g 0 0 100 100 re f /Sh0 sh", SHADING);
        let VectorItem::Raster { ops, bounds } = &page.draws[1].item else {
            unreachable!()
        };
        assert_eq!(bounds, &[0.0, 0.0, 200.0, 100.0]);
        let pixmap = rasterize_ops(&list, ops.clone(), &Matrix::IDENTITY, 200, 100).unwrap();
        let samples = pixmap.samples();
        let pixel = |x: usize, y: usize| &samples[(y * 200 + x) * 4..(y * 200 + x) * 4 + 4];
        // The shading covers the page; the black fill is not part of the layer.
        assert!(
            pixel(5, 50)[0] > 200,
            "left edge should be red: {:?}",
            pixel(5, 50)
        );
        assert!(
            pixel(195, 50)[2] > 200,
            "right edge should be blue: {:?}",
            pixel(195, 50)
        );
        assert_eq!(pixel(50, 50)[3], 255);
    }

    #[test]
    fn raster_layer_numbering_survives_operations_outside_the_pixmap() {
        // The first fill lies outside the 100x100 pixmap below.
        let (page, list) = capture("0 g 150 10 10 10 re f /Sh0 sh", SHADING);
        let VectorItem::Raster { ops, .. } = &page.draws[1].item else {
            unreachable!()
        };
        let pixmap = rasterize_ops(&list, ops.clone(), &Matrix::IDENTITY, 100, 100).unwrap();
        assert_eq!(
            pixmap.samples()[(50 * 100 + 50) * 4 + 3],
            255,
            "shading missing"
        );
    }

    /// A 2×1 inline image (red, blue) stretched over the page's left half.
    const IMAGE: &str =
        "q 100 0 0 100 0 0 cm BI /W 2 /H 1 /CS /RGB /BPC 8 /F /AHx ID ff00000000ff> EI Q";

    #[test]
    fn images_keep_their_pixels_and_land_where_mupdf_draws_them() {
        let (page, list) = capture(IMAGE, "<< >>");
        assert_eq!(kinds(&page), ["image"]);
        let VectorItem::Image {
            rgba,
            size,
            transform,
            ..
        } = &page.draws[0].item
        else {
            unreachable!()
        };
        assert_eq!(size, &[2, 1]);
        assert_eq!(&rgba.0[..], &[255, 0, 0, 255, 0, 0, 255, 255]);
        // The centre of pixel (0, 0) through the transform…
        let [a, b, c, d, e, f] = *transform;
        let (u, v) = (0.25, 0.5);
        let (x, y) = (a * u + c * v + e, b * u + d * v + f);
        // …is red in MuPDF's own raster of the page.
        let mut pixmap = Pixmap::new_with_w_h(&Colorspace::device_rgb(), 200, 100, false).unwrap();
        pixmap.clear_with(255).unwrap();
        let device = Device::from_pixmap(&pixmap).unwrap();
        list.run(&device, &Matrix::IDENTITY, Rect::INF).unwrap();
        drop(device);
        let offset = (y as usize * 200 + x as usize) * 3;
        assert_eq!(&pixmap.samples()[offset..offset + 3], &[255, 0, 0]);
    }

    #[test]
    fn images_in_blend_groups_are_rasterized() {
        let content = format!("/G gs {IMAGE}");
        let (page, _) = capture(&content, "<< /ExtGState << /G << /BM /Multiply >> >> >>");
        assert_eq!(page.stats.images, 0);
        assert!(
            page.draws
                .iter()
                .all(|draw| !matches!(draw.item, VectorItem::Image { .. }))
        );
    }

    #[test]
    fn image_masks_paint_their_colour_where_mupdf_does() {
        // A 2×1 stencil (first pixel painted under the default decode) in red.
        let (page, list) = capture(
            "1 0 0 rg q 100 0 0 100 0 0 cm BI /W 2 /H 1 /IM true /BPC 1 /F /AHx ID 40> EI Q",
            "<< >>",
        );
        let VectorItem::Image {
            rgba, transform, ..
        } = &page.draws[0].item
        else {
            panic!("expected an image, got {:?}", kinds(&page));
        };
        let [a, b, c, d, e, f] = *transform;
        let mut pixmap = Pixmap::new_with_w_h(&Colorspace::device_rgb(), 200, 100, false).unwrap();
        pixmap.clear_with(255).unwrap();
        let device = Device::from_pixmap(&pixmap).unwrap();
        list.run(&device, &Matrix::IDENTITY, Rect::INF).unwrap();
        drop(device);
        for (pixel, u) in [(0usize, 0.25f32), (1, 0.75)] {
            let (x, y) = (a * u + c * 0.5 + e, b * u + d * 0.5 + f);
            let offset = (y as usize * 200 + x as usize) * 3;
            let painted = pixmap.samples()[offset + 1] < 128;
            assert_eq!(rgba.0[pixel * 4 + 3] > 128, painted, "pixel {pixel}");
            if painted {
                assert_eq!(&rgba.0[pixel * 4..pixel * 4 + 4], &[255, 0, 0, 255]);
            }
        }
    }
}
