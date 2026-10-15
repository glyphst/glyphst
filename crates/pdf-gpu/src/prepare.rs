//! CPU preparation of a captured page into GPU-ready buffers.
//!
//! Geometry stays in page space so zooming only changes a uniform. Fills
//! become one quad per path whose pixels integrate the path's coverage from
//! its flattened edges (as HEPR draws them), indexed by horizontal bands when
//! long; fills too dense for that, and clip paths, are tessellated once per
//! level of detail. Strokes become analytic segment instances whose width
//! the shader widens to at least one device pixel.
//! Coarser levels flatten and simplify geometry so that, at the largest scale
//! each level serves, it differs from the exact shape by at most
//! [`LOD_ERROR_PX`]. Every level also drops opaque strokes that repaint what
//! the same batch already painted at that level's precision, so drawings that
//! repeat or densely dot their hatching stay cheap to draw.

use std::{collections::HashSet, ops::Range};

use bytemuck::{Pod, Zeroable};
use glyphst_pdf::vector::{
    LineCap, PagePath, PageRect, PathVerb, StrokeStyle, VectorItem, VectorPage,
};
use lyon_tessellation::{
    BuffersBuilder, FillOptions, FillRule, FillTessellator, FillVertex as LyonFillVertex,
    VertexBuffers,
    geom::{CubicBezierSegment, point},
    path::Path as LyonPath,
};

/// Curve flattening tolerance of the exact level in page units (points).
/// 0.01 pt stays within half a device pixel up to 50 pixels per point.
pub const FLATTEN_TOLERANCE: f32 = 0.01;

/// Largest geometric error a level shows, in device pixels.
pub const LOD_ERROR_PX: f32 = 0.5;

/// Largest device pixels per page point each coarse level serves; above the
/// last one the exact level is used.
const LOD_MAX_SCALES: [f32; 6] = [1.0 / 16.0, 0.25, 1.0, 4.0, 16.0, 64.0];

/// A coarse level is kept only when it has at most this fraction of the
/// primitives of the next finer level.
const LOD_MIN_REDUCTION: f64 = 0.9;

pub const SEGMENT_BUTT_START: u32 = 1;
pub const SEGMENT_BUTT_END: u32 = 2;
/// The record is no segment but the coverage fill [`StrokeSegment::path`],
/// drawn in its place in paint order.
pub const SEGMENT_FILL: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct FillVertex {
    pub position: [f32; 2],
    /// Straight-alpha RGBA8.
    pub color: [u8; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct StrokeSegment {
    pub p0: [f32; 2],
    pub p1: [f32; 2],
    /// Half the line width in page units; zero for hairlines.
    pub half_width: f32,
    /// Straight-alpha RGBA8.
    pub color: [u8; 4],
    pub flags: u32,
    /// With [`SEGMENT_FILL`]: the index into
    /// [`PreparedLevel::coverage_paths`].
    pub path: u32,
}

impl StrokeSegment {
    /// The record drawing coverage fill `path`.
    fn fill(path: u32) -> Self {
        Self {
            flags: SEGMENT_FILL,
            path,
            ..Self::zeroed()
        }
    }
}

/// A fill drawn as one quad whose pixels average the path's winding over
/// their footprint (see `fs_coverage`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct CoveragePath {
    /// Page-space bounds of its edges.
    pub bounds: [f32; 4],
    /// First edge in [`PreparedLevel::coverage_segments`] and their count.
    pub segments: [u32; 2],
    /// First band in [`PreparedLevel::bands`] and their count; without bands
    /// a pixel sums every edge.
    pub bands: [u32; 2],
    /// Straight-alpha RGBA8.
    pub color: [u8; 4],
    /// 1 for the even-odd rule, 0 for nonzero.
    pub even_odd: u32,
    /// Signed area enclosed by the edges, in square points, for pixels
    /// larger than the whole path.
    pub area: f32,
    pub _padding: u32,
}

/// What became of a fill offered as a coverage fill.
enum Coverage {
    /// Added as this path.
    Path(u32),
    /// It encloses nothing.
    Empty,
    /// Too dense or sparse for coverage: tessellate it.
    Tessellate,
}

/// A flattened fill edge; horizontal edges add nothing and are left out.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct CoverageSegment {
    pub p0: [f32; 2],
    pub p1: [f32; 2],
}

/// Fills with more edges than this get bands, so a pixel sums only the
/// edges crossing its rows.
const BANDED_SEGMENTS: usize = 8;
/// Edges a band aims to hold, and the most bands a fill gets.
const SEGMENTS_PER_BAND: usize = 4;
const MAX_BANDS: usize = 512;
/// A fill whose fullest band would hold more edges is tessellated instead:
/// each of its pixels would sum too many.
const MAX_BAND_SEGMENTS: usize = 32;
/// Fills larger than this (in points) covering less than `SPARSE_SHARE` of
/// their bounds are tessellated instead.
const SPARSE_MIN_SIDE: f32 = 20.0;
const SPARSE_SHARE: f64 = 0.2;
/// Coverage data a level keeps under one storage binding (wgpu's default
/// limit is 128 MiB); later fills are tessellated.
const MAX_COVERAGE_BYTES: usize = 64 << 20;

#[derive(Clone, Debug, PartialEq)]
pub enum BatchKind {
    Fills(Range<u32>),
    /// Records of [`PreparedLevel::segments`] in paint order: stroke
    /// segments and coverage fills ([`SEGMENT_FILL`]), drawn by one
    /// pipeline.
    Vectors(Range<u32>),
    /// Index into [`PreparedPage::raster_layers`].
    Raster(usize),
    /// Index into [`PreparedPage::images`].
    Image(usize),
    /// Fill indices of a clip path that raises the stencil depth to the
    /// batch's depth inside the path, and the index of the matching pop.
    ClipPush(Range<u32>, usize),
    /// The same clip path, lowering the stencil depth from the batch's depth.
    ClipPop(Range<u32>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Batch {
    pub kind: BatchKind,
    pub clip: Option<PageRect>,
    /// Stencil clip depth: content draws where the stencil equals it.
    pub depth: u32,
    /// Consecutive sub-ranges of a fill or stroke batch with their page
    /// bounds, so off-screen work can be skipped without changing order.
    pub chunks: Vec<Chunk>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    pub range: Range<u32>,
    pub bounds: PageRect,
}

/// Primitives (triangles or segments) per culling chunk.
const CHUNK_PRIMITIVES: u32 = 512;

/// Display-list operations the host rasterizes with
/// [`glyphst_pdf::vector::rasterize_ops`], and the page area they can paint.
#[derive(Clone, Debug, PartialEq)]
pub struct RasterLayerSource {
    pub ops: Range<u32>,
    pub bounds: PageRect,
}

#[derive(Clone, Debug, Default)]
pub struct PreparedPage {
    pub bounds: PageRect,
    /// Coarse to exact; see [`PreparedPage::level_for_scale`].
    pub levels: Vec<PreparedLevel>,
    pub raster_layers: Vec<RasterLayerSource>,
    /// Images at their own resolution, shared by every level.
    pub images: Vec<PreparedImage>,
}

/// An image to draw as a textured quad; see [`VectorItem::Image`].
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedImage {
    pub rgba: glyphst_pdf::vector::ImagePixels,
    pub size: [u32; 2],
    pub transform: [f32; 6],
    pub interpolate: bool,
    /// The rest of the mip chain, box-filtered from `rgba`: each level's
    /// pixels and size, halving down to one pixel.
    pub mips: Vec<(Vec<u8>, [u32; 2])>,
}

impl PreparedImage {
    /// Page-space corners: top-left, top-right, bottom-left, bottom-right
    /// of the image's pixel grid.
    pub fn corners(&self) -> [[f32; 2]; 4] {
        let [a, b, c, d, e, f] = self.transform;
        [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
            .map(|(u, v)| [a * u + c * v + e, b * u + d * v + f])
    }
}

/// Page geometry at one level of detail.
#[derive(Clone, Debug, Default)]
pub struct PreparedLevel {
    /// Largest device pixels per page point this level serves.
    pub max_scale: f32,
    pub tolerance: f32,
    pub fill_vertices: Vec<FillVertex>,
    pub fill_indices: Vec<u32>,
    pub segments: Vec<StrokeSegment>,
    pub coverage_paths: Vec<CoveragePath>,
    pub coverage_segments: Vec<CoverageSegment>,
    /// Per band: first entry in `band_entries` and their count.
    pub bands: Vec<[u32; 2]>,
    /// Indices into `coverage_segments`.
    pub band_entries: Vec<u32>,
    pub batches: Vec<Batch>,
    pub failed_fills: usize,
    /// Stencil depth while building batches.
    depth: u32,
}

impl PreparedPage {
    pub fn new(page: &VectorPage) -> Self {
        let images = page
            .draws
            .iter()
            .filter_map(|draw| match &draw.item {
                VectorItem::Image {
                    rgba,
                    size,
                    transform,
                    interpolate,
                } => Some(PreparedImage {
                    rgba: rgba.clone(),
                    size: *size,
                    transform: *transform,
                    interpolate: *interpolate,
                    mips: mip_chain(&rgba.0, *size),
                }),
                _ => None,
            })
            .collect();
        let raster_layers = page
            .draws
            .iter()
            .filter_map(|draw| match &draw.item {
                VectorItem::Raster { ops, bounds } => Some(RasterLayerSource {
                    ops: ops.clone(),
                    bounds: *bounds,
                }),
                _ => None,
            })
            .collect();
        // Levels are independent; build them in parallel, finest first.
        let built: Vec<PreparedLevel> = std::thread::scope(|scope| {
            let handles: Vec<_> = std::iter::once(None)
                .chain(LOD_MAX_SCALES.iter().rev().copied().map(Some))
                .map(|max_scale| {
                    scope.spawn(move || match max_scale {
                        Some(max_scale) => {
                            PreparedLevel::new(page, max_scale, LOD_ERROR_PX / max_scale)
                        }
                        None => PreparedLevel::new(page, f32::INFINITY, FLATTEN_TOLERANCE),
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("level preparation panicked"))
                .collect()
        });
        // A level that saves little over the next finer one is dropped; the
        // finer level then serves its scales.
        let mut levels: Vec<PreparedLevel> = Vec::new();
        for level in built {
            let saves_enough = levels.last().is_none_or(|finer| {
                level.primitives() as f64 <= finer.primitives() as f64 * LOD_MIN_REDUCTION
            });
            if saves_enough {
                levels.push(level);
            }
        }
        levels.reverse();
        Self {
            bounds: page.bounds,
            levels,
            raster_layers,
            images,
        }
    }

    /// Index of the coarsest level exact enough for `scale` device pixels
    /// per page point.
    pub fn level_for_scale(levels: &[f32], scale: f32) -> usize {
        levels
            .iter()
            .position(|&max_scale| scale <= max_scale)
            .unwrap_or(levels.len().saturating_sub(1))
    }

    pub fn exact(&self) -> &PreparedLevel {
        self.levels
            .last()
            .expect("prepared pages have an exact level")
    }
}

impl PreparedLevel {
    fn new(page: &VectorPage, max_scale: f32, tolerance: f32) -> Self {
        let mut level = Self {
            max_scale,
            tolerance,
            ..Self::default()
        };
        let mut tessellator = FillTessellator::new();
        let mut polylines = Vec::new();
        let mut raster_layer = 0;
        let mut image = 0;
        // Index ranges and push batches of the clips on the stencil stack.
        let mut clips: Vec<(Range<u32>, usize)> = Vec::new();
        for draw in &page.draws {
            level.depth = clips.len() as u32;
            match &draw.item {
                VectorItem::Fill {
                    path,
                    even_odd,
                    color,
                } => {
                    match level.push_coverage(path, *even_odd, *color, &mut polylines) {
                        Coverage::Path(index) => {
                            let start = level.segments.len() as u32;
                            level.segments.push(StrokeSegment::fill(index));
                            level.push_batch(BatchKind::Vectors(start..start + 1), draw.clip);
                            continue;
                        }
                        Coverage::Empty => continue,
                        Coverage::Tessellate => {}
                    }
                    let start = level.fill_indices.len() as u32;
                    if level
                        .tessellate_fill(&mut tessellator, path, *even_odd, *color)
                        .is_err()
                    {
                        level.failed_fills += 1;
                    }
                    let end = level.fill_indices.len() as u32;
                    level.push_batch(BatchKind::Fills(start..end), draw.clip);
                }
                VectorItem::Stroke { path, style, color } => {
                    let start = level.segments.len() as u32;
                    flatten(path, tolerance, &mut polylines);
                    for polyline in &mut polylines {
                        if tolerance > FLATTEN_TOLERANCE {
                            simplify(&mut polyline.points, tolerance);
                        }
                        level.push_stroke(polyline, style, *color);
                    }
                    let end = level.segments.len() as u32;
                    level.push_batch(BatchKind::Vectors(start..end), draw.clip);
                }
                VectorItem::Raster { .. } => {
                    level.batches.push(Batch {
                        kind: BatchKind::Raster(raster_layer),
                        clip: draw.clip,
                        depth: level.depth,
                        chunks: Vec::new(),
                    });
                    raster_layer += 1;
                }
                VectorItem::Image { .. } => {
                    level.batches.push(Batch {
                        kind: BatchKind::Image(image),
                        clip: draw.clip,
                        depth: level.depth,
                        chunks: Vec::new(),
                    });
                    image += 1;
                }
                VectorItem::ClipPush { path, even_odd } => {
                    let start = level.fill_indices.len() as u32;
                    if level
                        .tessellate_fill(&mut tessellator, path, *even_odd, [0; 4])
                        .is_err()
                    {
                        level.failed_fills += 1;
                    }
                    let range = start..level.fill_indices.len() as u32;
                    clips.push((range.clone(), level.batches.len()));
                    level.batches.push(Batch {
                        // The pop index is filled in when the clip pops.
                        kind: BatchKind::ClipPush(range, usize::MAX),
                        clip: draw.clip,
                        depth: clips.len() as u32,
                        chunks: Vec::new(),
                    });
                }
                VectorItem::ClipPop => {
                    if let Some((range, push)) = clips.pop() {
                        let pop = level.batches.len();
                        if let BatchKind::ClipPush(_, pair) = &mut level.batches[push].kind {
                            *pair = pop;
                        }
                        level.batches.push(Batch {
                            kind: BatchKind::ClipPop(range),
                            clip: draw.clip,
                            depth: clips.len() as u32 + 1,
                            chunks: Vec::new(),
                        });
                    }
                }
            }
        }
        level.depth = 0;
        level.merge_repeated_marks(max_scale.is_finite().then(|| LOD_ERROR_PX / max_scale));
        for index in 0..level.batches.len() {
            level.batches[index].chunks = level.chunks(&level.batches[index].kind);
        }
        level
    }

    /// Removes opaque segments that repaint what an earlier segment of the
    /// same batch, color and width already painted, as seen at this level:
    /// marks no larger than `cell` that share a `cell`-sized cell, and other
    /// segments whose endpoints share cells. With no `cell` (the exact level)
    /// only identical segments are removed.
    fn merge_repeated_marks(&mut self, cell: Option<f32>) {
        let mut kept = Vec::with_capacity(self.segments.len());
        let mut seen = HashSet::new();
        let quantize = |value: f32| match cell {
            Some(cell) => (value / cell).floor() as i64,
            None => i64::from(value.to_bits()),
        };
        for batch in &mut self.batches {
            let BatchKind::Vectors(range) = &mut batch.kind else {
                continue;
            };
            seen.clear();
            let start = kept.len() as u32;
            for segment in &self.segments[range.start as usize..range.end as usize] {
                // A fill may paint over earlier marks: one after it repeats
                // none of them.
                if segment.flags & SEGMENT_FILL != 0 {
                    seen.clear();
                    kept.push(*segment);
                    continue;
                }
                // Overlapping translucent marks darken, so they all stay.
                if segment.color[3] < u8::MAX {
                    kept.push(*segment);
                    continue;
                }
                let [x0, y0] = segment.p0;
                let [x1, y1] = segment.p1;
                let tiny = cell.is_some_and(|cell| {
                    (x1 - x0).abs().max((y1 - y0).abs()) < cell && segment.half_width < cell
                });
                let key = if tiny {
                    let (x, y) = (quantize((x0 + x1) / 2.0), quantize((y0 + y1) / 2.0));
                    [x, y, x, y]
                } else {
                    [quantize(x0), quantize(y0), quantize(x1), quantize(y1)]
                };
                if seen.insert((key, segment.color, segment.half_width.to_bits())) {
                    kept.push(*segment);
                }
            }
            *range = start..kept.len() as u32;
        }
        self.segments = kept;
    }

    fn primitives(&self) -> usize {
        self.fill_indices.len() / 3 + self.segments.len() + self.coverage_segments.len()
    }

    /// Adds `path` as a coverage fill, unless it encloses nothing or is
    /// better tessellated.
    fn push_coverage(
        &mut self,
        path: &PagePath,
        even_odd: bool,
        color: [u8; 4],
        polylines: &mut Vec<Polyline>,
    ) -> Coverage {
        flatten(path, self.tolerance, polylines);
        let first = self.coverage_segments.len();
        let mut bounds: Option<PageRect> = None;
        let mut area = 0.0f64;
        let mut edges = Vec::new();
        for polyline in polylines.iter() {
            let points = &polyline.points;
            if points.len() < 2 {
                continue;
            }
            // Fills close every subpath.
            for (index, &p0) in points.iter().enumerate() {
                let p1 = points[(index + 1) % points.len()];
                edges.push((p0, p1));
            }
        }
        for &(p0, p1) in &edges {
            bounds = Some(union(
                bounds.unwrap_or([p0[0], p0[1], p0[0], p0[1]]),
                [
                    p0[0].min(p1[0]),
                    p0[1].min(p1[1]),
                    p0[0].max(p1[0]),
                    p0[1].max(p1[1]),
                ],
            ));
        }
        let Some(bounds) = bounds else {
            return Coverage::Empty;
        };
        // Green's theorem, relative to the corner for precision.
        let [ox, oy] = [f64::from(bounds[0]), f64::from(bounds[1])];
        for &(p0, p1) in &edges {
            let (x0, y0) = (f64::from(p0[0]) - ox, f64::from(p0[1]) - oy);
            let (x1, y1) = (f64::from(p1[0]) - ox, f64::from(p1[1]) - oy);
            area += (x0 * y1 - x1 * y0) / 2.0;
            if p0[1] != p1[1] {
                self.coverage_segments.push(CoverageSegment { p0, p1 });
            }
        }
        let count = self.coverage_segments.len() - first;
        if count == 0 {
            return Coverage::Empty;
        }
        // A large fill covering little of its bounds (a long diagonal
        // wall) would shade mostly empty pixels of its quad.
        let box_area = f64::from(bounds[2] - bounds[0]) * f64::from(bounds[3] - bounds[1]);
        let large = (bounds[2] - bounds[0]).max(bounds[3] - bounds[1]) > SPARSE_MIN_SIDE;
        if large && area.abs() < box_area * SPARSE_SHARE {
            self.coverage_segments.truncate(first);
            return Coverage::Tessellate;
        }
        let bands = if count > BANDED_SEGMENTS {
            match band(&self.coverage_segments[first..], first as u32, bounds) {
                Some(bands) => bands,
                None => {
                    self.coverage_segments.truncate(first);
                    return Coverage::Tessellate;
                }
            }
        } else {
            Vec::new()
        };
        let band_start = self.bands.len() as u32;
        for entries in &bands {
            self.bands
                .push([self.band_entries.len() as u32, entries.len() as u32]);
            self.band_entries.extend_from_slice(entries);
        }
        self.coverage_paths.push(CoveragePath {
            bounds,
            segments: [first as u32, count as u32],
            bands: [band_start, bands.len() as u32],
            color,
            even_odd: u32::from(even_odd),
            area: area as f32,
            _padding: 0,
        });
        if self.coverage_bytes() > MAX_COVERAGE_BYTES {
            self.coverage_paths.pop();
            self.coverage_segments.truncate(first);
            self.bands.truncate(band_start as usize);
            let entries = self.bands.last().map_or(0, |[start, count]| start + count);
            self.band_entries.truncate(entries as usize);
            return Coverage::Tessellate;
        }
        Coverage::Path(self.coverage_paths.len() as u32 - 1)
    }

    fn coverage_bytes(&self) -> usize {
        size_of_val(self.coverage_paths.as_slice())
            .max(size_of_val(self.coverage_segments.as_slice()))
            .max(size_of_val(self.band_entries.as_slice()))
    }

    fn chunks(&self, kind: &BatchKind) -> Vec<Chunk> {
        let mut chunks = Vec::new();
        match kind {
            BatchKind::Fills(range) => {
                for start in (range.start..range.end).step_by((CHUNK_PRIMITIVES * 3) as usize) {
                    let end = (start + CHUNK_PRIMITIVES * 3).min(range.end);
                    let bounds = self.fill_indices[start as usize..end as usize]
                        .iter()
                        .map(|&index| {
                            let [x, y] = self.fill_vertices[index as usize].position;
                            [x, y, x, y]
                        })
                        .reduce(union)
                        .unwrap_or_default();
                    chunks.push(Chunk {
                        range: start..end,
                        bounds,
                    });
                }
            }
            BatchKind::Vectors(range) => {
                for start in (range.start..range.end).step_by(CHUNK_PRIMITIVES as usize) {
                    let end = (start + CHUNK_PRIMITIVES).min(range.end);
                    let bounds = self.segments[start as usize..end as usize]
                        .iter()
                        .map(|segment| {
                            if segment.flags & SEGMENT_FILL != 0 {
                                return self.coverage_paths[segment.path as usize].bounds;
                            }
                            let w = segment.half_width;
                            [
                                segment.p0[0].min(segment.p1[0]) - w,
                                segment.p0[1].min(segment.p1[1]) - w,
                                segment.p0[0].max(segment.p1[0]) + w,
                                segment.p0[1].max(segment.p1[1]) + w,
                            ]
                        })
                        .reduce(union)
                        .unwrap_or_default();
                    chunks.push(Chunk {
                        range: start..end,
                        bounds,
                    });
                }
            }
            BatchKind::ClipPush(range, _) | BatchKind::ClipPop(range) => {
                let bounds = self.fill_indices[range.start as usize..range.end as usize]
                    .iter()
                    .map(|&index| {
                        let [x, y] = self.fill_vertices[index as usize].position;
                        [x, y, x, y]
                    })
                    .reduce(union)
                    .unwrap_or_default();
                chunks.push(Chunk {
                    range: range.clone(),
                    bounds,
                });
            }
            BatchKind::Raster(_) | BatchKind::Image(_) => {}
        }
        chunks
    }

    /// Appends `kind`, merging it into the previous batch when both draw the
    /// same primitive under the same clip with contiguous ranges.
    fn push_batch(&mut self, kind: BatchKind, clip: Option<PageRect>) {
        let empty = match &kind {
            BatchKind::Fills(range) | BatchKind::Vectors(range) => range.is_empty(),
            BatchKind::Raster(_)
            | BatchKind::Image(_)
            | BatchKind::ClipPush(..)
            | BatchKind::ClipPop(_) => false,
        };
        if empty {
            return;
        }
        if let Some(last) = self.batches.last_mut()
            && last.clip == clip
            && last.depth == self.depth
        {
            match (&mut last.kind, &kind) {
                (BatchKind::Fills(previous), BatchKind::Fills(next))
                | (BatchKind::Vectors(previous), BatchKind::Vectors(next))
                    if previous.end == next.start =>
                {
                    previous.end = next.end;
                    return;
                }
                _ => {}
            }
        }
        self.batches.push(Batch {
            kind,
            clip,
            depth: self.depth,
            chunks: Vec::new(),
        });
    }

    fn tessellate_fill(
        &mut self,
        tessellator: &mut FillTessellator,
        path: &PagePath,
        even_odd: bool,
        color: [u8; 4],
    ) -> Result<(), lyon_tessellation::TessellationError> {
        let path = lyon_path(path);
        let options = FillOptions::tolerance(self.tolerance).with_fill_rule(if even_odd {
            FillRule::EvenOdd
        } else {
            FillRule::NonZero
        });
        let mut buffers = VertexBuffers::<FillVertex, u32>::new();
        tessellator.tessellate_path(
            &path,
            &options,
            &mut BuffersBuilder::new(&mut buffers, |vertex: LyonFillVertex| FillVertex {
                position: vertex.position().to_array(),
                color,
            }),
        )?;
        let base = self.fill_vertices.len() as u32;
        self.fill_vertices.extend_from_slice(&buffers.vertices);
        self.fill_indices
            .extend(buffers.indices.iter().map(|index| base + index));
        Ok(())
    }

    fn push_stroke(&mut self, polyline: &Polyline, style: &StrokeStyle, color: [u8; 4]) {
        let half_width = style.width / 2.0;
        if style.dashes.iter().any(|dash| *dash > 0.0) {
            for dash in dash_polyline(polyline, &style.dashes, style.dash_phase) {
                self.push_open(&dash, half_width, style.dash_cap, style.dash_cap, color);
            }
        } else if polyline.closed {
            let points = &polyline.points;
            for index in 0..points.len() {
                let next = points[(index + 1) % points.len()];
                if points.len() > 1 && next == points[index] {
                    continue;
                }
                self.segments
                    .push(segment(points[index], next, half_width, color, 0));
            }
        } else {
            self.push_open(
                &polyline.points,
                half_width,
                style.start_cap,
                style.end_cap,
                color,
            );
        }
    }

    fn push_open(
        &mut self,
        points: &[[f32; 2]],
        half_width: f32,
        start_cap: LineCap,
        end_cap: LineCap,
        color: [u8; 4],
    ) {
        let Some((&first, rest)) = points.split_first() else {
            return;
        };
        if rest.is_empty() {
            // A zero-length subpath draws only its caps.
            if start_cap == LineCap::Round {
                self.segments
                    .push(segment(first, first, half_width, color, 0));
            }
            return;
        }
        let start = self.segments.len();
        let mut previous = first;
        for &point in rest {
            if point != previous {
                self.segments
                    .push(segment(previous, point, half_width, color, 0));
            }
            previous = point;
        }
        let end = self.segments.len();
        if start == end {
            return;
        }
        apply_cap(&mut self.segments[start], start_cap, true);
        apply_cap(&mut self.segments[end - 1], end_cap, false);
    }
}

/// Splits a fill's edges (indices from `first`) into horizontal bands of
/// `bounds`, each listing the edges crossing its rows; `None` when a band
/// would hold more than [`MAX_BAND_SEGMENTS`]. The shader finds a pixel's
/// bands with the same arithmetic, in `f32`.
fn band(segments: &[CoverageSegment], first: u32, bounds: PageRect) -> Option<Vec<Vec<u32>>> {
    let count = segments
        .len()
        .div_ceil(SEGMENTS_PER_BAND)
        .clamp(1, MAX_BANDS);
    let height = (bounds[3] - bounds[1]) / count as f32;
    let mut bands = vec![Vec::new(); count];
    if height <= 0.0 {
        return None;
    }
    // A little slack, so rounding never leaves an edge out of a band whose
    // rows it touches.
    let slack = height * 1e-3;
    let index = |y: f32| {
        ((y - bounds[1]) / height)
            .floor()
            .clamp(0.0, (count - 1) as f32) as usize
    };
    for (offset, segment) in segments.iter().enumerate() {
        let low = segment.p0[1].min(segment.p1[1]) - slack;
        let high = segment.p0[1].max(segment.p1[1]) + slack;
        for entries in &mut bands[index(low)..=index(high)] {
            if entries.len() == MAX_BAND_SEGMENTS {
                return None;
            }
            entries.push(first + offset as u32);
        }
    }
    Some(bands)
}

fn union(a: PageRect, b: PageRect) -> PageRect {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

fn segment(
    p0: [f32; 2],
    p1: [f32; 2],
    half_width: f32,
    color: [u8; 4],
    flags: u32,
) -> StrokeSegment {
    StrokeSegment {
        p0,
        p1,
        half_width,
        color,
        flags,
        path: 0,
    }
}

/// Round caps come from the capsule shape; butt caps are cut in the shader;
/// square caps extend the end point by half the width and are then cut.
fn apply_cap(segment: &mut StrokeSegment, cap: LineCap, start: bool) {
    let (flag, end, other) = if start {
        (SEGMENT_BUTT_START, &mut segment.p0, segment.p1)
    } else {
        (SEGMENT_BUTT_END, &mut segment.p1, segment.p0)
    };
    match cap {
        LineCap::Round => {}
        LineCap::Butt => segment.flags |= flag,
        LineCap::Square => {
            let dx = end[0] - other[0];
            let dy = end[1] - other[1];
            let length = (dx * dx + dy * dy).sqrt();
            if length > 0.0 {
                end[0] += dx / length * segment.half_width;
                end[1] += dy / length * segment.half_width;
            }
            segment.flags |= flag;
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Polyline {
    points: Vec<[f32; 2]>,
    closed: bool,
}

fn flatten(path: &PagePath, tolerance: f32, out: &mut Vec<Polyline>) {
    out.clear();
    let mut points = path.points.iter().copied();
    let mut current = Polyline::default();
    let mut subpath_start = [0.0, 0.0];
    for verb in &path.verbs {
        match verb {
            PathVerb::MoveTo => {
                let Some(point) = points.next() else { break };
                if !current.points.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                subpath_start = point;
                current.points.push(point);
            }
            PathVerb::LineTo => {
                let Some(point) = points.next() else { break };
                if current.points.is_empty() {
                    current.points.push(subpath_start);
                }
                current.points.push(point);
            }
            PathVerb::CubicTo => {
                let (Some(c1), Some(c2), Some(to)) = (points.next(), points.next(), points.next())
                else {
                    break;
                };
                if current.points.is_empty() {
                    current.points.push(subpath_start);
                }
                let from = *current.points.last().unwrap_or(&subpath_start);
                CubicBezierSegment {
                    from: point(from[0], from[1]),
                    ctrl1: point(c1[0], c1[1]),
                    ctrl2: point(c2[0], c2[1]),
                    to: point(to[0], to[1]),
                }
                .for_each_flattened(tolerance, &mut |line| {
                    current.points.push(line.to.to_array());
                });
            }
            PathVerb::Close => {
                if !current.points.is_empty() {
                    if current.points.len() > 1 && current.points.last() == current.points.first() {
                        current.points.pop();
                    }
                    current.closed = true;
                    out.push(std::mem::take(&mut current));
                }
            }
        }
    }
    if !current.points.is_empty() {
        out.push(current);
    }
}

/// Douglas-Peucker simplification keeping every point farther than
/// `tolerance` from the simplified line; endpoints are always kept.
fn simplify(points: &mut Vec<[f32; 2]>, tolerance: f32) {
    if points.len() < 3 {
        return;
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
    let tolerance_squared = tolerance * tolerance;
    while let Some((first, last)) = stack.pop() {
        let [ax, ay] = points[first];
        let [bx, by] = points[last];
        let (dx, dy) = (bx - ax, by - ay);
        let length_squared = dx * dx + dy * dy;
        let mut farthest = (0.0, first);
        for (index, &[px, py]) in points.iter().enumerate().take(last).skip(first + 1) {
            let distance_squared = if length_squared > 0.0 {
                let cross = (px - ax) * dy - (py - ay) * dx;
                cross * cross / length_squared
            } else {
                (px - ax) * (px - ax) + (py - ay) * (py - ay)
            };
            if distance_squared > farthest.0 {
                farthest = (distance_squared, index);
            }
        }
        if farthest.0 > tolerance_squared {
            keep[farthest.1] = true;
            stack.push((first, farthest.1));
            stack.push((farthest.1, last));
        }
    }
    let mut index = 0;
    points.retain(|_| {
        index += 1;
        keep[index - 1]
    });
}

/// Splits a polyline into dashes; a closed polyline is dashed as if open
/// with its closing segment appended.
fn dash_polyline(polyline: &Polyline, dashes: &[f32], phase: f32) -> Vec<Vec<[f32; 2]>> {
    let mut points = polyline.points.clone();
    if polyline.closed
        && let Some(&first) = points.first()
    {
        points.push(first);
    }
    let pattern_length: f32 = dashes.iter().sum();
    let mut index = 0;
    let mut remaining = dashes[0];
    let mut on = true;
    let mut skip = phase.rem_euclid(pattern_length.max(f32::EPSILON));
    while skip > 0.0 {
        if skip >= remaining {
            skip -= remaining;
            index = (index + 1) % dashes.len();
            remaining = dashes[index];
            on = !on;
        } else {
            remaining -= skip;
            skip = 0.0;
        }
    }

    let mut out = Vec::new();
    let mut current: Vec<[f32; 2]> = Vec::new();
    if on && let Some(&first) = points.first() {
        current.push(first);
    }
    for window in points.windows(2) {
        let [from, to] = [window[0], window[1]];
        let dx = to[0] - from[0];
        let dy = to[1] - from[1];
        let length = (dx * dx + dy * dy).sqrt();
        let mut travelled = 0.0;
        while length - travelled > remaining {
            travelled += remaining;
            let t = travelled / length;
            let point = [from[0] + dx * t, from[1] + dy * t];
            if on {
                current.push(point);
                out.push(std::mem::take(&mut current));
            } else {
                current.push(point);
            }
            on = !on;
            index = (index + 1) % dashes.len();
            remaining = dashes[index];
        }
        remaining -= length - travelled;
        if on {
            current.push(to);
        }
    }
    if on && current.len() > 1 {
        out.push(current);
    }
    out
}

fn ensure_open(
    builder: &mut lyon_tessellation::path::path::Builder,
    open: &mut bool,
    start: [f32; 2],
) {
    if !*open {
        builder.begin(point(start[0], start[1]));
        *open = true;
    }
}

fn lyon_path(path: &PagePath) -> LyonPath {
    let mut builder = LyonPath::builder();
    let mut points = path.points.iter().copied();
    let mut open = false;
    let mut start = [0.0, 0.0];
    for verb in &path.verbs {
        match verb {
            PathVerb::MoveTo => {
                let Some(point_value) = points.next() else {
                    break;
                };
                if open {
                    builder.end(false);
                    open = false;
                }
                start = point_value;
                ensure_open(&mut builder, &mut open, start);
            }
            PathVerb::LineTo => {
                let Some(to) = points.next() else { break };
                ensure_open(&mut builder, &mut open, start);
                builder.line_to(point(to[0], to[1]));
            }
            PathVerb::CubicTo => {
                let (Some(c1), Some(c2), Some(to)) = (points.next(), points.next(), points.next())
                else {
                    break;
                };
                ensure_open(&mut builder, &mut open, start);
                builder.cubic_bezier_to(
                    point(c1[0], c1[1]),
                    point(c2[0], c2[1]),
                    point(to[0], to[1]),
                );
            }
            PathVerb::Close => {
                if open {
                    builder.end(true);
                    open = false;
                }
            }
        }
    }
    if open {
        builder.end(false);
    }
    builder.build()
}

/// Mip levels 1.. of an RGBA8 image of `size` (level 0 is the image).
pub fn mip_chain(rgba: &[u8], size: [u32; 2]) -> Vec<(Vec<u8>, [u32; 2])> {
    let count = size[0].max(size[1]).max(1).ilog2();
    let mut mips: Vec<(Vec<u8>, [u32; 2])> = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let (pixels, [width, height]) = match mips.last() {
            Some((pixels, size)) => (pixels.as_slice(), *size),
            None => (rgba, size),
        };
        let (half, half_width, half_height) = downsample(pixels, width, height);
        mips.push((half, [half_width, half_height]));
    }
    mips
}

/// Halves an RGBA8 image (2x2 box filter; odd edges repeat).
pub(crate) fn downsample(rgba: &[u8], width: u32, height: u32) -> (Vec<u8>, u32, u32) {
    let (half_width, half_height) = ((width / 2).max(1), (height / 2).max(1));
    let mut out = Vec::with_capacity((half_width * half_height * 4) as usize);
    let pixel = |x: u32, y: u32, channel: usize| {
        u32::from(rgba[((y.min(height - 1) * width + x.min(width - 1)) * 4) as usize + channel])
    };
    for y in 0..half_height {
        for x in 0..half_width {
            for channel in 0..4 {
                let sum = pixel(2 * x, 2 * y, channel)
                    + pixel(2 * x + 1, 2 * y, channel)
                    + pixel(2 * x, 2 * y + 1, channel)
                    + pixel(2 * x + 1, 2 * y + 1, channel);
                out.push(((sum + 2) / 4) as u8);
            }
        }
    }
    (out, half_width, half_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(verbs: &[PathVerb], points: &[[f32; 2]]) -> PagePath {
        PagePath {
            verbs: verbs.to_vec(),
            points: points.to_vec(),
        }
    }

    #[test]
    fn closed_polylines_drop_the_repeated_start_point() {
        use PathVerb::*;
        let mut out = Vec::new();
        flatten(
            &path(
                &[MoveTo, LineTo, LineTo, LineTo, Close],
                &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 0.0]],
            ),
            FLATTEN_TOLERANCE,
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].closed);
        assert_eq!(out[0].points, [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]);
    }

    #[test]
    fn curves_flatten_within_tolerance() {
        use PathVerb::*;
        let mut out = Vec::new();
        flatten(
            &path(
                &[MoveTo, CubicTo],
                &[[0.0, 0.0], [0.0, 100.0], [100.0, 100.0], [100.0, 0.0]],
            ),
            FLATTEN_TOLERANCE,
            &mut out,
        );
        let points = &out[0].points;
        assert!(points.len() > 20, "{} points", points.len());
        assert_eq!(points.last(), Some(&[100.0, 0.0]));
    }

    #[test]
    fn dashes_split_lines_and_respect_phase() {
        let line = Polyline {
            points: vec![[0.0, 0.0], [10.0, 0.0]],
            closed: false,
        };
        let dashes = dash_polyline(&line, &[2.0, 3.0], 0.0);
        assert_eq!(
            dashes,
            [vec![[0.0, 0.0], [2.0, 0.0]], vec![[5.0, 0.0], [7.0, 0.0]],]
        );
        let shifted = dash_polyline(&line, &[2.0, 3.0], 1.0);
        assert_eq!(shifted[0], vec![[0.0, 0.0], [1.0, 0.0]]);
        assert_eq!(shifted[1], vec![[4.0, 0.0], [6.0, 0.0]]);
    }

    #[test]
    fn caps_set_flags_and_square_caps_extend() {
        let mut page = PreparedLevel::default();
        let style = StrokeStyle {
            width: 2.0,
            start_cap: LineCap::Square,
            end_cap: LineCap::Butt,
            dash_cap: LineCap::Butt,
            dashes: Vec::new(),
            dash_phase: 0.0,
        };
        page.push_stroke(
            &Polyline {
                points: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]],
                closed: false,
            },
            &style,
            [0, 0, 0, 255],
        );
        assert_eq!(page.segments.len(), 2);
        assert_eq!(page.segments[0].p0, [-1.0, 0.0]);
        assert_eq!(page.segments[0].flags, SEGMENT_BUTT_START);
        assert_eq!(page.segments[1].flags, SEGMENT_BUTT_END);
    }

    #[test]
    fn same_clip_batches_merge_and_raster_layers_split_them() {
        use glyphst_pdf::vector::VectorDraw;
        let square = path(
            &[
                PathVerb::MoveTo,
                PathVerb::LineTo,
                PathVerb::LineTo,
                PathVerb::Close,
            ],
            &[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
        );
        let fill = VectorItem::Fill {
            path: square,
            even_odd: false,
            color: [0, 0, 0, 255],
        };
        let draws = [
            fill.clone(),
            fill.clone(),
            VectorItem::Raster {
                ops: 3..4,
                bounds: [0.0; 4],
            },
            fill,
        ]
        .into_iter()
        .map(|item| VectorDraw { item, clip: None })
        .collect();
        let prepared = PreparedPage::new(&VectorPage {
            draws,
            ..VectorPage::default()
        });
        assert_eq!(
            prepared
                .exact()
                .batches
                .iter()
                .map(|batch| &batch.kind)
                .collect::<Vec<_>>(),
            [
                &BatchKind::Vectors(0..2),
                &BatchKind::Raster(0),
                &BatchKind::Vectors(2..3),
            ]
        );
    }

    fn line(x: f32) -> VectorItem {
        VectorItem::Stroke {
            path: path(
                &[PathVerb::MoveTo, PathVerb::LineTo],
                &[[x, 0.0], [x, 10.0]],
            ),
            style: StrokeStyle {
                width: 1.0,
                start_cap: LineCap::Butt,
                end_cap: LineCap::Butt,
                dash_cap: LineCap::Butt,
                dashes: Vec::new(),
                dash_phase: 0.0,
            },
            color: [0, 0, 0, 255],
        }
    }

    #[test]
    fn strokes_and_fills_share_a_batch_in_paint_order_until_the_clip_changes() {
        use glyphst_pdf::vector::VectorDraw;
        let square = VectorItem::Fill {
            path: polygon(&[[0.0, 0.0], [5.0, 0.0], [5.0, 5.0], [0.0, 5.0]]),
            even_odd: false,
            color: [255, 0, 0, 255],
        };
        let clip = Some([0.0, 0.0, 3.0, 3.0]);
        let draws = vec![
            VectorDraw {
                item: line(1.0),
                clip: None,
            },
            VectorDraw {
                item: square.clone(),
                clip: None,
            },
            VectorDraw {
                item: line(2.0),
                clip: None,
            },
            VectorDraw { item: square, clip },
        ];
        let prepared = PreparedPage::new(&VectorPage {
            draws,
            ..VectorPage::default()
        });
        let level = prepared.exact();
        let kinds: Vec<_> = level.batches.iter().map(|batch| &batch.kind).collect();
        assert_eq!(
            kinds,
            [&BatchKind::Vectors(0..3), &BatchKind::Vectors(3..4)]
        );
        let fills: Vec<bool> = level
            .segments
            .iter()
            .map(|segment| segment.flags & SEGMENT_FILL != 0)
            .collect();
        assert_eq!(fills, [false, true, false, true]);
        assert_eq!(level.segments[3].path, 1);
    }

    #[test]
    fn a_mark_repeated_over_a_fill_is_kept() {
        use glyphst_pdf::vector::VectorDraw;
        let cover = VectorItem::Fill {
            path: polygon(&[[0.0, 0.0], [5.0, 0.0], [5.0, 10.0], [0.0, 10.0]]),
            even_odd: false,
            color: [255, 255, 255, 255],
        };
        let draws = [line(1.0), line(1.0), cover, line(1.0)]
            .into_iter()
            .map(|item| VectorDraw { item, clip: None })
            .collect();
        let prepared = PreparedPage::new(&VectorPage {
            draws,
            ..VectorPage::default()
        });
        // The second line repeats the first; the third is painted again
        // over the fill.
        assert_eq!(prepared.exact().segments.len(), 3);
    }

    fn fill(path: PagePath) -> VectorPage {
        VectorPage {
            draws: vec![glyphst_pdf::vector::VectorDraw {
                item: VectorItem::Fill {
                    path,
                    even_odd: false,
                    color: [0, 0, 0, 255],
                },
                clip: None,
            }],
            ..VectorPage::default()
        }
    }

    fn polygon(points: &[[f32; 2]]) -> PagePath {
        let mut verbs = vec![PathVerb::LineTo; points.len()];
        verbs[0] = PathVerb::MoveTo;
        verbs.push(PathVerb::Close);
        path(&verbs, points)
    }

    #[test]
    fn coverage_fills_keep_their_sloped_edges_and_signed_area() {
        let square = PreparedPage::new(&fill(polygon(&[
            [10.0, 10.0],
            [30.0, 10.0],
            [30.0, 20.0],
            [10.0, 20.0],
        ])));
        let level = square.exact();
        assert_eq!(level.fill_indices.len(), 0, "not tessellated");
        let path = level.coverage_paths[0];
        assert_eq!(path.bounds, [10.0, 10.0, 30.0, 20.0]);
        // The horizontal edges add nothing to any pixel's winding.
        assert_eq!(path.segments, [0, 2]);
        assert_eq!(path.bands, [0, 0]);
        assert_eq!(path.area.abs(), 200.0);

        let triangle = PreparedPage::new(&fill(polygon(&[[0.0, 0.0], [4.0, 0.0], [0.0, 3.0]])));
        assert_eq!(triangle.exact().coverage_paths[0].area.abs(), 6.0);
    }

    #[test]
    fn long_fills_get_bands_that_list_the_edges_crossing_them() {
        // A zigzag: 100 sloped edges down the left, one edge back up.
        let mut points: Vec<[f32; 2]> = (0..=100)
            .map(|i| [if i % 2 == 0 { 0.0 } else { 5.0 }, i as f32])
            .collect();
        points.push([50.0, 100.0]);
        points.push([50.0, 0.0]);
        let prepared = PreparedPage::new(&fill(polygon(&points)));
        let level = prepared.exact();
        let path = level.coverage_paths[0];
        let [first, count] = path.bands;
        assert!(count > 1, "{count} bands");
        let height = (path.bounds[3] - path.bounds[1]) / count as f32;
        for (index, &[start, entries]) in level.bands[first as usize..][..count as usize]
            .iter()
            .enumerate()
        {
            let (low, high) = (index as f32 * height, (index + 1) as f32 * height);
            let listed = &level.band_entries[start as usize..][..entries as usize];
            assert!(listed.len() <= MAX_BAND_SEGMENTS);
            for (offset, edge) in level.coverage_segments.iter().enumerate() {
                let (y0, y1) = (edge.p0[1].min(edge.p1[1]), edge.p0[1].max(edge.p1[1]));
                if y0 < high && y1 > low {
                    assert!(
                        listed.contains(&(offset as u32)),
                        "band {index} misses {offset}"
                    );
                }
            }
        }
    }

    #[test]
    fn fills_too_dense_for_bands_are_tessellated() {
        // A comb of 200 teeth: every band would hold 400 edges.
        let mut points = Vec::new();
        for tooth in 0..200 {
            let x = tooth as f32 * 2.0;
            points.extend([[x, 0.0], [x, 10.0], [x + 1.0, 10.0], [x + 1.0, 0.0]]);
        }
        let prepared = PreparedPage::new(&fill(polygon(&points)));
        let level = prepared.exact();
        assert!(level.coverage_paths.is_empty());
        assert!(level.coverage_segments.is_empty());
        assert!(!level.fill_indices.is_empty());
        assert!(matches!(level.batches[0].kind, BatchKind::Fills(_)));
    }

    #[test]
    fn simplification_keeps_corners_and_drops_near_collinear_points() {
        let mut points = vec![[0.0, 0.0], [5.0, 0.05], [10.0, 0.0], [10.0, 10.0]];
        simplify(&mut points, 0.1);
        assert_eq!(points, [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]]);
    }

    #[test]
    fn levels_are_chosen_by_scale_and_coarser_levels_are_smaller() {
        let max_scales = [0.25, 1.0, f32::INFINITY];
        assert_eq!(PreparedPage::level_for_scale(&max_scales, 0.1), 0);
        assert_eq!(PreparedPage::level_for_scale(&max_scales, 0.5), 1);
        assert_eq!(PreparedPage::level_for_scale(&max_scales, 100.0), 2);

        let circle = path(
            &[
                PathVerb::MoveTo,
                PathVerb::CubicTo,
                PathVerb::CubicTo,
                PathVerb::Close,
            ],
            &[
                [0.0, 50.0],
                [0.0, 0.0],
                [100.0, 0.0],
                [100.0, 50.0],
                [100.0, 100.0],
                [0.0, 100.0],
                [0.0, 50.0],
            ],
        );
        let stroke = glyphst_pdf::vector::VectorDraw {
            item: VectorItem::Stroke {
                path: circle,
                style: StrokeStyle {
                    width: 1.0,
                    start_cap: LineCap::Butt,
                    end_cap: LineCap::Butt,
                    dash_cap: LineCap::Butt,
                    dashes: Vec::new(),
                    dash_phase: 0.0,
                },
                color: [0, 0, 0, 255],
            },
            clip: None,
        };
        let prepared = PreparedPage::new(&VectorPage {
            draws: vec![stroke],
            ..VectorPage::default()
        });
        let counts: Vec<_> = prepared
            .levels
            .iter()
            .map(|level| level.segments.len())
            .collect();
        assert!(
            counts.windows(2).all(|pair| pair[0] <= pair[1]),
            "{counts:?}"
        );
        assert!(counts[0] < counts[counts.len() - 1], "{counts:?}");
    }

    #[test]
    fn coarse_levels_merge_overlapping_dots_but_keep_distinct_ones() {
        let dot = |x: f32, color: [u8; 4]| glyphst_pdf::vector::VectorDraw {
            item: VectorItem::Stroke {
                path: path(
                    &[PathVerb::MoveTo, PathVerb::LineTo],
                    &[[x, 10.0], [x + 0.01, 10.0]],
                ),
                style: StrokeStyle {
                    width: 0.1,
                    start_cap: LineCap::Round,
                    end_cap: LineCap::Round,
                    dash_cap: LineCap::Round,
                    dashes: Vec::new(),
                    dash_phase: 0.0,
                },
                color,
            },
            clip: None,
        };
        let black = [0, 0, 0, 255];
        let draws = vec![
            dot(10.0, black),
            dot(10.001, black),
            dot(10.002, [255, 0, 0, 255]),
            dot(50.0, black),
        ];
        let prepared = PreparedPage::new(&VectorPage {
            draws,
            ..VectorPage::default()
        });
        // At 1 px/pt the cell is 0.5 pt: the two nearby black dots merge.
        let level = &prepared.levels[PreparedPage::level_for_scale(
            &prepared
                .levels
                .iter()
                .map(|level| level.max_scale)
                .collect::<Vec<_>>(),
            1.0,
        )];
        assert_eq!(level.segments.len(), 3);
        assert_eq!(prepared.exact().segments.len(), 4);
        assert!(
            prepared.levels.len() < LOD_MAX_SCALES.len() + 1,
            "similar levels are dropped"
        );
    }

    #[test]
    fn downsampling_averages_two_by_two_blocks_and_repeats_odd_edges() {
        let rgba = [
            0, 0, 0, 0, 100, 100, 100, 100, 200, 200, 200, 200, //
            100, 100, 100, 100, 0, 0, 0, 0, 40, 40, 40, 40,
        ];
        let (half, width, height) = downsample(&rgba, 3, 2);
        assert_eq!((width, height), (1, 1));
        assert_eq!(half, [50, 50, 50, 50]);
        let (single, width, height) = downsample(&[9, 9, 9, 9], 1, 1);
        assert_eq!(
            (single.as_slice(), width, height),
            (&[9u8, 9, 9, 9][..], 1, 1)
        );
    }

    #[test]
    fn mip_chains_halve_down_to_one_pixel() {
        let sizes: Vec<[u32; 2]> = mip_chain(&[7; 5 * 3 * 4], [5, 3])
            .into_iter()
            .map(|(pixels, size)| {
                assert_eq!(pixels.len() as u32, size[0] * size[1] * 4);
                size
            })
            .collect();
        assert_eq!(sizes, [[2, 1], [1, 1]]);
        assert!(mip_chain(&[0; 4], [1, 1]).is_empty());
    }
}
