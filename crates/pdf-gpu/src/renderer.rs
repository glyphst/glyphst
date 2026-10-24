use std::{
    borrow::Borrow,
    ops::Range,
    sync::{Mutex, PoisonError},
};

use bytemuck::{Pod, Zeroable};
use glyphst_pdf::vector::PageRect;
use wgpu::util::DeviceExt as _;

use crate::{
    OffscreenTarget,
    prepare::{
        Batch, BatchKind, CoveragePath, FillVertex, PreparedImage, PreparedPage, StrokeSegment,
    },
};

pub const SAMPLE_COUNT: u32 = 4;

/// Render passes drawing pages need a stencil attachment of this format with
/// [`SAMPLE_COUNT`] samples, cleared to zero; non-rectangular clips use it.
pub const STENCIL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Stencil8;
/// The stencil bit a caller sets to keep pages out of some pixels: every
/// page batch tests its clip depth with this bit clear unless a draw asks
/// for it ([`DrawOptions::stencil`]), so pixels marked with it show only
/// drawings made for them. Clip depths stay below it.
pub const STENCIL_REVEAL_BIT: u32 = 0x80;
/// Camera uniforms a page holds, so one page drawn several times in a frame
/// (at different opacities) keeps each drawing's own; the last is the
/// page's overview tiles'.
pub const CAMERA_SLOTS: u32 = 8;
/// Bytes between camera slots: wgpu's uniform offset alignment.
const CAMERA_STRIDE: u64 = 256;

/// How [`Renderer::draw_with`] draws a page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawOptions {
    /// Only the part of the page inside this rectangle (page points).
    pub clip: Option<PageRect>,
    /// The whole drawing's opacity, 0 to 1.
    pub opacity: f32,
    /// Stencil bits the drawing's pixels must carry
    /// ([`STENCIL_REVEAL_BIT`] or 0).
    pub stencil: u32,
    /// The camera slot, below `CAMERA_SLOTS - 1`; drawings of one page in
    /// one frame each take their own.
    pub slot: u32,
}

impl Default for DrawOptions {
    fn default() -> Self {
        Self {
            clip: None,
            opacity: 1.0,
            stencil: 0,
            slot: 0,
        }
    }
}

/// Tile size used when rendering the overview texture; one tile is the unit
/// of work an interactive host does per frame.
const OVERVIEW_TILE: u32 = 1024;

/// An overview being built tile by tile (see [`Renderer::begin_overview`]).
pub struct OverviewJob {
    texture: wgpu::Texture,
    tile: OffscreenTarget,
    scale: f32,
    size: [u32; 2],
    mip_level_count: u32,
    /// The next tile: mip level and its top-left pixel.
    next: (u32, u32, u32),
}

/// Pages with at least this many stroke segments are expensive to draw
/// zoomed out, where every segment covers at least a pixel and they pile up;
/// they get an overview. Fill triangles cost far less (a dense text page
/// has a few hundred thousand and draws in well under a millisecond), so
/// only a much larger count of them asks for one.
const OVERVIEW_MIN_SEGMENTS: usize = 100_000;
const OVERVIEW_MIN_TRIANGLES: usize = 2_000_000;

/// Maps page space to device pixels:
/// `pixel = (rotate(page, rotation) - origin) * scale`, where `origin` is in
/// the rotated (view) space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub origin: [f32; 2],
    pub scale: f32,
    /// Viewport size in device pixels.
    pub viewport: [u32; 2],
    /// Quarter turns clockwise of the page on screen (0-3).
    pub rotation: u8,
}

/// `point` turned `quarter_turns` clockwise about the origin, with y down.
pub fn rotate(point: [f32; 2], quarter_turns: u8) -> [f32; 2] {
    let [m00, m01, m10, m11] = axes(quarter_turns);
    [
        m00 * point[0] + m01 * point[1],
        m10 * point[0] + m11 * point[1],
    ]
}

/// The rotation as a row-major 2×2 matrix.
fn axes(quarter_turns: u8) -> [f32; 4] {
    match quarter_turns % 4 {
        0 => [1.0, 0.0, 0.0, 1.0],
        1 => [0.0, -1.0, 1.0, 0.0],
        2 => [-1.0, 0.0, 0.0, -1.0],
        _ => [0.0, 1.0, -1.0, 0.0],
    }
}

/// The bounding box of `rect` turned `quarter_turns` clockwise.
pub fn rotate_rect(rect: PageRect, quarter_turns: u8) -> PageRect {
    let [a, b] = [
        rotate([rect[0], rect[1]], quarter_turns),
        rotate([rect[2], rect[3]], quarter_turns),
    ];
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[0].max(b[0]),
        a[1].max(b[1]),
    ]
}

impl Camera {
    /// Centers `focus` (page space) in the viewport at `scale`.
    /// Shows all of `bounds`, centred, in `viewport`.
    pub fn fitting(bounds: PageRect, viewport: [u32; 2]) -> Self {
        let [x0, y0, x1, y1] = bounds;
        let scale = (viewport[0] as f32 / (x1 - x0).max(1e-3))
            .min(viewport[1] as f32 / (y1 - y0).max(1e-3));
        Self::centered([(x0 + x1) / 2.0, (y0 + y1) / 2.0], scale, viewport)
    }

    pub fn centered(focus: [f32; 2], scale: f32, viewport: [u32; 2]) -> Self {
        Self::centered_rotated(focus, scale, viewport, 0)
    }

    /// Centers `focus` (page space) in the viewport at `scale`, with the
    /// page turned `rotation` quarter turns clockwise.
    pub fn centered_rotated(focus: [f32; 2], scale: f32, viewport: [u32; 2], rotation: u8) -> Self {
        let focus = rotate(focus, rotation);
        Self {
            origin: [
                focus[0] - viewport[0] as f32 / 2.0 / scale,
                focus[1] - viewport[1] as f32 / 2.0 / scale,
            ],
            scale,
            viewport,
            rotation: rotation % 4,
        }
    }

    /// The row-major page-to-view matrix.
    pub fn axes(&self) -> [f32; 4] {
        axes(self.rotation)
    }

    /// The device pixel of a page point.
    pub fn to_pixels(&self, page: [f32; 2]) -> [f32; 2] {
        let view = rotate(page, self.rotation);
        [
            (view[0] - self.origin[0]) * self.scale,
            (view[1] - self.origin[1]) * self.scale,
        ]
    }

    /// The page point at a device pixel.
    pub fn to_page(&self, pixels: [f32; 2]) -> [f32; 2] {
        let view = [
            self.origin[0] + pixels[0] / self.scale,
            self.origin[1] + pixels[1] / self.scale,
        ];
        rotate(view, (4 - self.rotation % 4) % 4)
    }

    /// The page-space rectangle visible in the viewport, widened by `margin`
    /// device pixels to cover hairlines widened in the shader.
    pub fn visible_page_rect(&self, margin: f32) -> PageRect {
        let margin = margin / self.scale;
        let view = [
            self.origin[0] - margin,
            self.origin[1] - margin,
            self.origin[0] + self.viewport[0] as f32 / self.scale + margin,
            self.origin[1] + self.viewport[1] as f32 / self.scale + margin,
        ];
        rotate_rect(view, (4 - self.rotation % 4) % 4)
    }

    /// Page-space rectangle to a pixel scissor clamped to the viewport.
    pub fn scissor(&self, rect: PageRect) -> Option<[u32; 4]> {
        // An empty rectangle (a clip beside the page) stays empty.
        if rect[2] <= rect[0] || rect[3] <= rect[1] {
            return None;
        }
        let rect = rotate_rect(rect, self.rotation);
        let x0 = ((rect[0] - self.origin[0]) * self.scale).floor().max(0.0);
        let y0 = ((rect[1] - self.origin[1]) * self.scale).floor().max(0.0);
        let x1 = ((rect[2] - self.origin[0]) * self.scale)
            .ceil()
            .min(self.viewport[0] as f32);
        let y1 = ((rect[3] - self.origin[1]) * self.scale)
            .ceil()
            .min(self.viewport[1] as f32);
        (x1 > x0 && y1 > y0).then_some([x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32])
    }
}

/// Work recorded by one [`Renderer::draw`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrawStats {
    pub draw_calls: usize,
    pub triangles: usize,
    pub segments: usize,
    pub used_overview: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    origin: [f32; 2],
    scale: f32,
    opacity: f32,
    viewport: [f32; 2],
    _padding2: [f32; 2],
    /// Row-major page-to-view rotation.
    axes: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LayerVertex {
    position: [f32; 2],
    uv: [f32; 2],
}

pub struct Renderer {
    format: wgpu::TextureFormat,
    camera_layout: wgpu::BindGroupLayout,
    layer_layout: wgpu::BindGroupLayout,
    coverage_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    mip_sampler: wgpu::Sampler,
    /// Images without /Interpolate: square pixels when enlarged, mipmaps
    /// when reduced.
    pixel_sampler: wgpu::Sampler,
    fill: wgpu::RenderPipeline,
    /// Stroke segments and coverage fills.
    vectors: wgpu::RenderPipeline,
    /// The coverage bind group of levels without coverage fills.
    no_coverage: wgpu::BindGroup,
    layer: wgpu::RenderPipeline,
    clip_push: wgpu::RenderPipeline,
    clip_pop: wgpu::RenderPipeline,
    mark: wgpu::RenderPipeline,
}

fn stencil_state(pass_op: wgpu::StencilOperation, write_mask: u32) -> wgpu::DepthStencilState {
    let face = wgpu::StencilFaceState {
        compare: wgpu::CompareFunction::Equal,
        fail_op: wgpu::StencilOperation::Keep,
        depth_fail_op: wgpu::StencilOperation::Keep,
        pass_op,
    };
    wgpu::DepthStencilState {
        format: STENCIL_FORMAT,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Always),
        stencil: wgpu::StencilState {
            front: face,
            back: face,
            read_mask: 0xff,
            write_mask,
        },
        bias: Default::default(),
    }
}

struct RasterLayer {
    vertices: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    rect: PageRect,
    /// Texture bytes counted in the page's `gpu_bytes`.
    bytes: u64,
}

/// The page rendered from its own vectors into a mipmapped texture, drawn
/// instead of the vectors whenever the camera scale is at most `scale`, so
/// the texture always has at least as many pixels as the screen.
struct Overview {
    layer: RasterLayer,
    scale: f32,
}

struct GpuLevel {
    max_scale: f32,
    fill_vertices: Option<wgpu::Buffer>,
    fill_indices: Option<wgpu::Buffer>,
    segments: Option<wgpu::Buffer>,
    /// The coverage fills' paths, edges, bands and band entries, and their
    /// bind group.
    coverage: Option<(Vec<wgpu::Buffer>, wgpu::BindGroup)>,
    batches: Vec<Batch>,
}

/// A prepared page resident on the GPU.
pub struct GpuPage {
    bounds: PageRect,
    camera: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    levels: Vec<GpuLevel>,
    max_scales: Vec<f32>,
    layers: Vec<Option<RasterLayer>>,
    images: Vec<Option<RasterLayer>>,
    overview: Option<Overview>,
    /// Exact-level stroke segments and fill triangles.
    segments: usize,
    triangles: usize,
    /// Levels written so far, coarsest first: while a page uploads it draws
    /// the finest of them, and nothing until the coarsest is written.
    ready_levels: usize,
    /// The visible work of the last frames, reused while the view stays
    /// inside it.
    selection: Mutex<Option<Selection>>,
    pub gpu_bytes: u64,
}

/// Device pixels around the view whose primitives a [`Selection`] keeps, so
/// pans and zooms within them reuse it (HEPR selects with a 256-pixel
/// margin for the same reason).
const SELECTION_MARGIN_PX: f32 = 256.0;

/// A selection is made again once the view zooms in this far past it, so a
/// wide one does not keep drawing what lies far off screen.
const SELECTION_MAX_ZOOM_IN: f32 = 2.0;

/// The chunks of one level's fill, stroke and clip batches that overlap
/// `rect`, the view with a margin, at `scale`.
struct Selection {
    level: usize,
    rect: PageRect,
    scale: f32,
    /// Per batch; raster and image batches are tested every frame.
    ranges: Vec<Vec<Range<u32>>>,
}

impl Selection {
    fn new(level_index: usize, level: &GpuLevel, camera: &Camera) -> Self {
        let rect = camera.visible_page_rect(SELECTION_MARGIN_PX);
        let ranges = level
            .batches
            .iter()
            .map(|batch| match &batch.kind {
                BatchKind::Fills(range)
                | BatchKind::Vectors(range)
                | BatchKind::ClipPush(range, _)
                | BatchKind::ClipPop(range) => visible_ranges(&batch.chunks, range, rect),
                BatchKind::Raster(_) | BatchKind::Image(_) => Vec::new(),
            })
            .collect();
        Self {
            level: level_index,
            rect,
            scale: camera.scale,
            ranges,
        }
    }

    /// Whether it still serves `level` seen through `camera`.
    fn serves(&self, level_index: usize, level: &GpuLevel, camera: &Camera) -> bool {
        self.level == level_index
            && self.ranges.len() == level.batches.len()
            && camera.scale <= self.scale * SELECTION_MAX_ZOOM_IN
            && covers(self.rect, camera.visible_page_rect(2.0))
    }
}

impl GpuPage {
    /// The deepest clip nesting of the page's levels. At
    /// [`STENCIL_REVEAL_BIT`] or deeper the reveal bit would collide with
    /// clip depths, and the page cannot be masked.
    pub fn max_clip_depth(&self) -> u32 {
        self.levels
            .iter()
            .flat_map(|level| &level.batches)
            .map(|batch| batch.depth)
            .max()
            .unwrap_or(0)
    }

    /// Largest camera scale the overview texture serves, if built.
    pub fn overview_scale(&self) -> Option<f32> {
        self.overview.as_ref().map(|overview| overview.scale)
    }

    /// Whether zoomed-out views of this page are dense enough to need an
    /// overview texture.
    pub fn wants_overview(&self) -> bool {
        self.segments >= OVERVIEW_MIN_SEGMENTS || self.triangles >= OVERVIEW_MIN_TRIANGLES
    }

    /// Index of the level drawn at `scale` device pixels per page point:
    /// the one made for it, or the finest written so far if that is coarser.
    pub fn level_for_scale(&self, scale: f32) -> usize {
        drawn_level(&self.max_scales, self.ready_levels, scale)
    }

    /// Whether every level has been written.
    pub fn is_complete(&self) -> bool {
        self.ready_levels == self.levels.len()
    }
}

/// The level drawn at `scale` when `ready` levels (coarsest first) are
/// written: the one made for the scale, or the finest written if coarser.
fn drawn_level(max_scales: &[f32], ready: usize, scale: f32) -> usize {
    PreparedPage::level_for_scale(max_scales, scale).min(ready.saturating_sub(1))
}

const PREMULTIPLIED: wgpu::BlendState = wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING;

impl Renderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::include_wgsl!("shaders.wgsl"));
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pdf_gpu_camera"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(size_of::<CameraUniform>() as u64),
                },
                count: None,
            }],
        });
        let layer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pdf_gpu_layer"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let coverage_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pdf_gpu_coverage"),
            entries: &[storage(0), storage(1), storage(2), storage(3)],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("pdf_gpu_layer"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let mip_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("pdf_gpu_overview"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let pixel_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("pdf_gpu_image_pixels"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        let pipeline = |name: &str,
                        vs: &str,
                        fs: &str,
                        layouts: &[Option<&wgpu::BindGroupLayout>],
                        buffers: &[Option<wgpu::VertexBufferLayout>],
                        topology: wgpu::PrimitiveTopology,
                        stencil: wgpu::DepthStencilState,
                        write_mask: wgpu::ColorWrites| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(name),
                bind_group_layouts: layouts,
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(name),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(vs),
                    buffers,
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(PREMULTIPLIED),
                        write_mask,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState {
                    topology,
                    ..Default::default()
                },
                depth_stencil: Some(stencil),
                multisample: wgpu::MultisampleState {
                    count: SAMPLE_COUNT,
                    mask: !0,
                    alpha_to_coverage_enabled: false,
                },
                multiview_mask: None,
                cache: None,
            })
        };

        let fill = pipeline(
            "pdf_gpu_fill",
            "vs_fill",
            "fs_fill",
            &[Some(&camera_layout)],
            &[Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<FillVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Unorm8x4],
            })],
            wgpu::PrimitiveTopology::TriangleList,
            stencil_state(wgpu::StencilOperation::Keep, 0),
            wgpu::ColorWrites::ALL,
        );
        let vectors = pipeline(
            "pdf_gpu_vectors",
            "vs_vector",
            "fs_vector",
            &[Some(&camera_layout), Some(&coverage_layout)],
            &[Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<StrokeSegment>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![
                    0 => Float32x2,
                    1 => Float32x2,
                    2 => Float32,
                    3 => Unorm8x4,
                    4 => Uint32,
                    5 => Uint32,
                ],
            })],
            wgpu::PrimitiveTopology::TriangleStrip,
            stencil_state(wgpu::StencilOperation::Keep, 0),
            wgpu::ColorWrites::ALL,
        );
        let layer = pipeline(
            "pdf_gpu_layer",
            "vs_layer",
            "fs_layer",
            &[Some(&camera_layout), Some(&layer_layout)],
            &[Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<LayerVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
            })],
            wgpu::PrimitiveTopology::TriangleStrip,
            stencil_state(wgpu::StencilOperation::Keep, 0),
            wgpu::ColorWrites::ALL,
        );

        let fill_layout = [Some(wgpu::VertexBufferLayout {
            array_stride: size_of::<FillVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Unorm8x4],
        })];
        let clip_push = pipeline(
            "pdf_gpu_clip_push",
            "vs_fill",
            "fs_fill",
            &[Some(&camera_layout)],
            &fill_layout,
            wgpu::PrimitiveTopology::TriangleList,
            stencil_state(wgpu::StencilOperation::IncrementClamp, 0xff),
            wgpu::ColorWrites::empty(),
        );
        let clip_pop = pipeline(
            "pdf_gpu_clip_pop",
            "vs_fill",
            "fs_fill",
            &[Some(&camera_layout)],
            &fill_layout,
            wgpu::PrimitiveTopology::TriangleList,
            stencil_state(wgpu::StencilOperation::DecrementClamp, 0xff),
            wgpu::ColorWrites::empty(),
        );

        // Marks pixels with the reveal bit (or clears it) whatever they
        // hold, drawing no color.
        let mark_face = wgpu::StencilFaceState {
            compare: wgpu::CompareFunction::Always,
            fail_op: wgpu::StencilOperation::Keep,
            depth_fail_op: wgpu::StencilOperation::Keep,
            pass_op: wgpu::StencilOperation::Replace,
        };
        let mark = pipeline(
            "pdf_gpu_mark",
            "vs_mark",
            "fs_mark",
            &[],
            &[Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<[f32; 2]>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x2],
            })],
            wgpu::PrimitiveTopology::TriangleList,
            wgpu::DepthStencilState {
                stencil: wgpu::StencilState {
                    front: mark_face,
                    back: mark_face,
                    read_mask: 0xff,
                    write_mask: STENCIL_REVEAL_BIT,
                },
                ..stencil_state(wgpu::StencilOperation::Keep, 0)
            },
            wgpu::ColorWrites::empty(),
        );

        let no_coverage = coverage_bind_group(device, &coverage_layout, &[0; 4]).1;
        Self {
            format,
            camera_layout,
            layer_layout,
            coverage_layout,
            sampler,
            mip_sampler,
            pixel_sampler,
            fill,
            vectors,
            no_coverage,
            layer,
            clip_push,
            clip_pop,
            mark,
        }
    }

    /// Sets (`set`) or clears [`STENCIL_REVEAL_BIT`] in the pixels of the
    /// triangles in `vertices` (clip-space `[f32; 2]` corners, three a
    /// triangle). Pages drawn afterwards skip marked pixels unless drawn
    /// with that bit ([`DrawOptions::stencil`]).
    pub fn mark(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        vertices: &wgpu::Buffer,
        count: u32,
        set: bool,
    ) {
        if count == 0 {
            return;
        }
        pass.set_pipeline(&self.mark);
        pass.set_stencil_reference(if set { STENCIL_REVEAL_BIT } else { 0 });
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.draw(0..count, 0..1);
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Uploads a prepared page: its levels' buffers, and its images as
    /// mipmapped textures at their own resolution, all at once.
    /// Interactive hosts use [`Renderer::begin_upload`].
    pub fn upload(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        page: &PreparedPage,
    ) -> GpuPage {
        let mut upload = self.begin_upload(device, page);
        let mut belt = wgpu::util::StagingBelt::new(device.clone(), UPLOAD_CHUNK);
        while !upload.step(device, queue, &mut belt, u64::MAX) {}
        self.finish_upload(upload)
    }

    /// Creates a page's buffers and textures; [`PageUpload::step`] fills
    /// them a chunk at a time, coarsest level first, and
    /// [`Renderer::finish_upload`] returns the page once they are full.
    /// Meanwhile [`PageUpload::page`] draws what is written: the coarsest
    /// level, images and raster layers come first, so a dense page shows
    /// soon and only gets sharper.
    pub fn begin_upload<P: Borrow<PreparedPage>>(
        &self,
        device: &wgpu::Device,
        page: P,
    ) -> PageUpload<P> {
        let prepared = page.borrow();
        let mut writes = Vec::new();
        let buffer = |label: &str, len: usize, usage: wgpu::BufferUsages| {
            (len > 0).then(|| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: len as u64,
                    usage: usage | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
        };
        let levels: Vec<GpuLevel> = prepared
            .levels
            .iter()
            .enumerate()
            .map(|(index, level)| {
                let sizes = [
                    size_of_val(level.fill_vertices.as_slice()),
                    size_of_val(level.fill_indices.as_slice()),
                    size_of_val(level.segments.as_slice()),
                    size_of_val(level.coverage_paths.as_slice()),
                    size_of_val(level.coverage_segments.as_slice()),
                    size_of_val(level.bands.as_slice()),
                    size_of_val(level.band_entries.as_slice()),
                ];
                for (part, len) in sizes.into_iter().enumerate() {
                    if len > 0 {
                        writes.push((
                            Write::Level { level: index, part },
                            UploadItem {
                                len: len as u64,
                                unit: wgpu::COPY_BUFFER_ALIGNMENT,
                            },
                        ));
                    }
                }
                GpuLevel {
                    max_scale: level.max_scale,
                    fill_vertices: buffer(
                        "pdf_gpu_fill_vertices",
                        sizes[0],
                        wgpu::BufferUsages::VERTEX,
                    ),
                    fill_indices: buffer(
                        "pdf_gpu_fill_indices",
                        sizes[1],
                        wgpu::BufferUsages::INDEX,
                    ),
                    segments: buffer("pdf_gpu_segments", sizes[2], wgpu::BufferUsages::VERTEX),
                    coverage: (sizes[3] > 0)
                        .then(|| coverage_bind_group(device, &self.coverage_layout, &sizes[3..])),
                    batches: level.batches.clone(),
                }
            })
            .collect();
        let textures: Vec<wgpu::Texture> = prepared
            .images
            .iter()
            .enumerate()
            .map(|(index, image)| {
                let mip_level_count = image.mips.len() as u32 + 1;
                for mip in 0..mip_level_count {
                    let [width, height] = image_mip(image, mip).1;
                    writes.push((
                        Write::Image { image: index, mip },
                        UploadItem {
                            len: u64::from(width) * u64::from(height) * 4,
                            unit: u64::from(width) * 4,
                        },
                    ));
                }
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("pdf_gpu_image"),
                    size: wgpu::Extent3d {
                        width: image.size[0],
                        height: image.size[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            })
            .collect();
        let (preview_items, level_ends) = plan_writes(&mut writes, levels.len());
        let (writes, items) = writes.into_iter().unzip();

        let mut image_bytes = 0;
        let images = prepared
            .images
            .iter()
            .zip(&textures)
            .map(|(image, texture)| {
                image_bytes += std::iter::once(image.rgba.0.len())
                    .chain(image.mips.iter().map(|(pixels, _)| pixels.len()))
                    .sum::<usize>() as u64;
                let sampler = if image.interpolate {
                    &self.mip_sampler
                } else {
                    &self.pixel_sampler
                };
                Some(self.textured_quad(device, texture, sampler, image.corners()))
            })
            .collect();
        let camera = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pdf_gpu_camera"),
            size: CAMERA_STRIDE * u64::from(CAMERA_SLOTS),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pdf_gpu_camera"),
            layout: &self.camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &camera,
                    offset: 0,
                    size: wgpu::BufferSize::new(size_of::<CameraUniform>() as u64),
                }),
            }],
        });
        let gpu_bytes = levels
            .iter()
            .flat_map(|level| {
                [&level.fill_vertices, &level.fill_indices, &level.segments]
                    .into_iter()
                    .flatten()
                    .chain(level.coverage.iter().flat_map(|(buffers, _)| buffers))
            })
            .map(wgpu::Buffer::size)
            .sum::<u64>()
            + image_bytes;
        let gpu = GpuPage {
            bounds: prepared.bounds,
            camera,
            camera_bind_group,
            max_scales: levels.iter().map(|level| level.max_scale).collect(),
            levels,
            layers: prepared.raster_layers.iter().map(|_| None).collect(),
            images,
            overview: None,
            segments: prepared.exact().segments.len(),
            triangles: prepared.exact().fill_indices.len() / 3
                + prepared.exact().coverage_segments.len(),
            ready_levels: 0,
            selection: Mutex::new(None),
            gpu_bytes,
        };
        PageUpload {
            page,
            gpu,
            textures,
            writes,
            items,
            preview_items,
            level_ends,
            cursor: UploadCursor::default(),
            layers: Vec::new(),
        }
    }

    /// The page of a finished upload (see [`PageUpload::step`]).
    pub fn finish_upload<P: Borrow<PreparedPage>>(&self, upload: PageUpload<P>) -> GpuPage {
        let mut page = upload.gpu;
        for layer in upload.layers {
            self.finish_layer(&mut page, layer);
        }
        page.ready_levels = page.levels.len();
        page
    }

    /// Creates the texture of raster layer `index`, premultiplied RGBA8
    /// covering `rect`; [`LayerUpload::step`] (or [`PageUpload::add_layer`])
    /// writes it and [`Renderer::finish_layer`] installs it.
    pub fn begin_layer(
        &self,
        device: &wgpu::Device,
        index: usize,
        rect: PageRect,
        rgba: Vec<u8>,
        size: [u32; 2],
    ) -> LayerUpload {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pdf_gpu_raster_layer"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut layer = self.raster_layer(device, &texture, &self.sampler, rect);
        layer.bytes = u64::from(size[0]) * u64::from(size[1]) * 4;
        LayerUpload {
            index,
            size,
            rgba,
            texture,
            layer: Some(layer),
            cursor: UploadCursor::default(),
        }
    }

    /// Installs a written raster layer on `page`, replacing (and freeing)
    /// the one it had.
    pub fn finish_layer(&self, page: &mut GpuPage, mut upload: LayerUpload) {
        upload.install(page);
    }

    /// Supplies raster layer `index` as premultiplied RGBA8 covering `rect`.
    #[allow(clippy::too_many_arguments)]
    pub fn set_raster_layer(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        page: &mut GpuPage,
        index: usize,
        rect: PageRect,
        rgba: &[u8],
        size: [u32; 2],
    ) {
        let extent = wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pdf_gpu_raster_layer"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size[0] * 4),
                rows_per_image: Some(size[1]),
            },
            extent,
        );
        let mut layer = self.raster_layer(device, &texture, &self.sampler, rect);
        layer.bytes = u64::from(size[0]) * u64::from(size[1]) * 4;
        page.gpu_bytes += layer.bytes;
        // Replacing a layer (re-rasterized for a closer view) frees the old.
        if let Some(old) = page.layers[index].replace(layer) {
            page.gpu_bytes = page.gpu_bytes.saturating_sub(old.bytes);
        }
    }

    fn raster_layer(
        &self,
        device: &wgpu::Device,
        texture: &wgpu::Texture,
        sampler: &wgpu::Sampler,
        rect: PageRect,
    ) -> RasterLayer {
        let [x0, y0, x1, y1] = rect;
        self.textured_quad(
            device,
            texture,
            sampler,
            [[x0, y0], [x1, y0], [x0, y1], [x1, y1]],
        )
    }

    /// A texture drawn over a page-space quadrilateral whose corners are
    /// the texture's top-left, top-right, bottom-left and bottom-right.
    fn textured_quad(
        &self,
        device: &wgpu::Device,
        texture: &wgpu::Texture,
        sampler: &wgpu::Sampler,
        corners: [[f32; 2]; 4],
    ) -> RasterLayer {
        let view = texture.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pdf_gpu_raster_layer"),
            layout: &self.layer_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        let uvs = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]];
        let vertices: Vec<LayerVertex> = corners
            .into_iter()
            .zip(uvs)
            .map(|(position, uv)| LayerVertex { position, uv })
            .collect();
        let rect = corners.iter().fold(
            [f32::MAX, f32::MAX, f32::MIN, f32::MIN],
            |[x0, y0, x1, y1], [x, y]| [x0.min(*x), y0.min(*y), x1.max(*x), y1.max(*y)],
        );
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pdf_gpu_raster_layer"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        RasterLayer {
            vertices,
            bind_group,
            rect,
            bytes: 0,
        }
    }

    /// Renders the page from its vectors (and raster layers) into a
    /// mipmapped texture whose longer side is `max_dimension` pixels, all at
    /// once. Every mip level is rendered from the vectors rather than
    /// downsampled. Interactive hosts use [`Renderer::begin_overview`].
    pub fn build_overview(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        page: &mut GpuPage,
        max_dimension: u32,
    ) {
        if let Some(mut job) = self.begin_overview(device, page, max_dimension) {
            while !self.continue_overview(device, queue, page, &mut job, usize::MAX) {}
        }
    }

    /// Starts building an overview in steps; `None` for an empty page.
    pub fn begin_overview(
        &self,
        device: &wgpu::Device,
        page: &GpuPage,
        max_dimension: u32,
    ) -> Option<OverviewJob> {
        let [x0, y0, x1, y1] = page.bounds;
        let (width_pt, height_pt) = (x1 - x0, y1 - y0);
        if width_pt <= 0.0 || height_pt <= 0.0 {
            return None;
        }
        let scale = max_dimension as f32 / width_pt.max(height_pt);
        let width = ((width_pt * scale).ceil() as u32).clamp(1, max_dimension);
        let height = ((height_pt * scale).ceil() as u32).clamp(1, max_dimension);
        let mip_level_count = width.max(height).ilog2() + 1;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pdf_gpu_overview"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let tile = OffscreenTarget::new(
            device,
            self.format,
            [OVERVIEW_TILE, OVERVIEW_TILE],
            wgpu::TextureUsages::COPY_SRC,
        );
        Some(OverviewJob {
            texture,
            tile,
            scale,
            size: [width, height],
            mip_level_count,
            next: (0, 0, 0),
        })
    }

    /// Renders up to `tiles` more tiles of `job`. When the last tile is
    /// done the overview is installed on `page` and `true` is returned.
    pub fn continue_overview(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        page: &mut GpuPage,
        job: &mut OverviewJob,
        tiles: usize,
    ) -> bool {
        let [x0, y0, _, _] = page.bounds;
        for _ in 0..tiles {
            let (mip, tile_x, tile_y) = job.next;
            if mip >= job.mip_level_count {
                break;
            }
            let mip_width = (job.size[0] >> mip).max(1);
            let mip_height = (job.size[1] >> mip).max(1);
            let mip_scale = job.scale / (1u32 << mip) as f32;
            let camera = Camera {
                origin: [
                    x0 + tile_x as f32 / mip_scale,
                    y0 + tile_y as f32 / mip_scale,
                ],
                scale: mip_scale,
                viewport: [OVERVIEW_TILE, OVERVIEW_TILE],
                rotation: 0,
            };
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = job.tile.begin_pass(&mut encoder, wgpu::Color::TRANSPARENT);
                self.draw_vectors(
                    queue,
                    page,
                    &camera,
                    DrawOptions {
                        slot: CAMERA_SLOTS - 1,
                        ..DrawOptions::default()
                    },
                    &mut pass,
                    &mut DrawStats::default(),
                );
            }
            encoder.copy_texture_to_texture(
                job.tile.texture().as_image_copy(),
                wgpu::TexelCopyTextureInfo {
                    texture: &job.texture,
                    mip_level: mip,
                    origin: wgpu::Origin3d {
                        x: tile_x,
                        y: tile_y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: OVERVIEW_TILE.min(mip_width - tile_x),
                    height: OVERVIEW_TILE.min(mip_height - tile_y),
                    depth_or_array_layers: 1,
                },
            );
            // The camera uniform is rewritten per tile, so each tile is
            // submitted before the next write.
            queue.submit([encoder.finish()]);
            job.next = if tile_x + OVERVIEW_TILE < mip_width {
                (mip, tile_x + OVERVIEW_TILE, tile_y)
            } else if tile_y + OVERVIEW_TILE < mip_height {
                (mip, 0, tile_y + OVERVIEW_TILE)
            } else {
                (mip + 1, 0, 0)
            };
        }
        if job.next.0 < job.mip_level_count {
            return false;
        }

        let [width, height] = job.size;
        let rect = [
            x0,
            y0,
            x0 + width as f32 / job.scale,
            y0 + height as f32 / job.scale,
        ];
        let layer = self.raster_layer(device, &job.texture, &self.mip_sampler, rect);
        page.gpu_bytes += u64::from(width) * u64::from(height) * 4 * 4 / 3;
        page.overview = Some(Overview {
            layer,
            scale: job.scale,
        });

        // Levels serving only scales the overview covers are never drawn.
        for level in &mut page.levels {
            if level.max_scale <= job.scale {
                let coverage = level.coverage.take().map(|(buffers, _)| buffers);
                for buffer in [
                    level.fill_vertices.take(),
                    level.fill_indices.take(),
                    level.segments.take(),
                ]
                .into_iter()
                .flatten()
                .chain(coverage.into_iter().flatten())
                {
                    page.gpu_bytes -= buffer.size();
                    buffer.destroy();
                }
                level.batches.clear();
            }
        }
        true
    }

    /// Draws all of `page` on white into a new texture that can be sampled
    /// (a thumbnail), `scale` pixels per point and at most `max_side`
    /// pixels long, without reading it back; a page with an overview draws
    /// that.
    pub fn render_thumbnail(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        page: &GpuPage,
        scale: f32,
        max_side: u32,
    ) -> wgpu::Texture {
        let [x0, y0, x1, y1] = page.bounds;
        let (width, height) = ((x1 - x0) * scale, (y1 - y0) * scale);
        let shrink = (max_side as f32 / width.max(height)).min(1.0);
        let size = [
            (width * shrink).round().max(1.0) as u32,
            (height * shrink).round().max(1.0) as u32,
        ];
        let target = OffscreenTarget::new(
            device,
            self.format,
            size,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
        );
        let camera = Camera::fitting(page.bounds, target.size());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = target.begin_pass(&mut encoder, wgpu::Color::WHITE);
            self.draw(queue, page, &camera, &mut pass);
        }
        queue.submit([encoder.finish()]);
        target.texture().clone()
    }

    /// Records the page into `pass`, which must target this renderer's format
    /// with [`SAMPLE_COUNT`] samples.
    pub fn draw(
        &self,
        queue: &wgpu::Queue,
        page: &GpuPage,
        camera: &Camera,
        pass: &mut wgpu::RenderPass<'_>,
    ) -> DrawStats {
        self.draw_with(queue, page, camera, DrawOptions::default(), pass)
    }

    /// [`Renderer::draw`] showing only the part of the page inside `clip`
    /// (page points): one drawing of a page revealing another.
    pub fn draw_clipped(
        &self,
        queue: &wgpu::Queue,
        page: &GpuPage,
        camera: &Camera,
        clip: PageRect,
        pass: &mut wgpu::RenderPass<'_>,
    ) -> DrawStats {
        let options = DrawOptions {
            clip: Some(clip),
            ..DrawOptions::default()
        };
        self.draw_with(queue, page, camera, options, pass)
    }

    /// [`Renderer::draw`] as `options` say: clipped, faded, or only into
    /// pixels carrying stencil bits.
    pub fn draw_with(
        &self,
        queue: &wgpu::Queue,
        page: &GpuPage,
        camera: &Camera,
        options: DrawOptions,
        pass: &mut wgpu::RenderPass<'_>,
    ) -> DrawStats {
        let mut stats = DrawStats::default();
        let bounds = options
            .clip
            .map_or(page.bounds, |clip| clip_to(page.bounds, clip));
        let options = DrawOptions {
            slot: options.slot.min(CAMERA_SLOTS - 2),
            ..options
        };
        match &page.overview {
            Some(overview) if camera.scale <= overview.scale => {
                self.write_camera(queue, page, camera, options);
                stats.used_overview = true;
                if let Some([x, y, width, height]) = camera.scissor(bounds) {
                    pass.set_bind_group(0, &page.camera_bind_group, &[camera_offset(options)]);
                    pass.set_scissor_rect(x, y, width, height);
                    pass.set_stencil_reference(options.stencil);
                    self.draw_layer(&overview.layer, pass);
                    stats.draw_calls += 1;
                }
            }
            _ => {
                let options = DrawOptions {
                    clip: Some(bounds),
                    ..options
                };
                self.draw_vectors(queue, page, camera, options, pass, &mut stats);
            }
        }
        stats
    }

    fn draw_layer(&self, layer: &RasterLayer, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.layer);
        pass.set_bind_group(1, &layer.bind_group, &[]);
        pass.set_vertex_buffer(0, layer.vertices.slice(..));
        pass.draw(0..4, 0..1);
    }

    fn write_camera(
        &self,
        queue: &wgpu::Queue,
        page: &GpuPage,
        camera: &Camera,
        options: DrawOptions,
    ) {
        queue.write_buffer(
            &page.camera,
            u64::from(camera_offset(options)),
            bytemuck::bytes_of(&CameraUniform {
                origin: camera.origin,
                scale: camera.scale,
                opacity: options.opacity.clamp(0.0, 1.0),
                viewport: [camera.viewport[0] as f32, camera.viewport[1] as f32],
                _padding2: [0.0; 2],
                axes: camera.axes(),
            }),
        );
    }

    fn draw_vectors(
        &self,
        queue: &wgpu::Queue,
        page: &GpuPage,
        camera: &Camera,
        options: DrawOptions,
        pass: &mut wgpu::RenderPass<'_>,
        stats: &mut DrawStats,
    ) {
        if page.ready_levels == 0 {
            return;
        }
        let bounds = options.clip.unwrap_or(page.bounds);
        self.write_camera(queue, page, camera, options);
        // Nothing shows outside the page box, as in MuPDF's pixmaps: EPUB
        // chapter backgrounds, for one, run past the pages they start on.
        let Some(full) = camera.scissor(bounds) else {
            return;
        };
        let level_index = page.level_for_scale(camera.scale);
        let level = &page.levels[level_index];
        let visible = camera.visible_page_rect(2.0);
        let mut selection = page
            .selection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let selection = match selection.take() {
            Some(kept) if kept.serves(level_index, level, camera) => selection.insert(kept),
            _ => selection.insert(Selection::new(level_index, level, camera)),
        };

        // Visible work per batch, then a running count of batches that draw
        // content, so a clip whose contents are all off screen is skipped.
        const WHOLE: &[Range<u32>] = std::slice::from_ref(&(0..1));
        let work: Vec<Option<BatchWork<'_>>> = level
            .batches
            .iter()
            .zip(&selection.ranges)
            .map(|(batch, selected)| {
                let scissor = match batch.clip {
                    Some(clip) => camera.scissor(clip_to(clip, bounds))?,
                    None => full,
                };
                let ranges: &[Range<u32>] = match &batch.kind {
                    BatchKind::Fills(_)
                    | BatchKind::Vectors(_)
                    | BatchKind::ClipPush(..)
                    | BatchKind::ClipPop(_) => selected,
                    BatchKind::Raster(index) => match page.layers.get(*index) {
                        Some(Some(layer)) if intersects(layer.rect, visible) => WHOLE,
                        _ => &[],
                    },
                    BatchKind::Image(index) => match page.images.get(*index) {
                        Some(Some(image)) if intersects(image.rect, visible) => WHOLE,
                        _ => &[],
                    },
                };
                (!ranges.is_empty()).then_some((scissor, ranges))
            })
            .collect();
        let mut content_before = Vec::with_capacity(work.len() + 1);
        content_before.push(0usize);
        for (batch, work) in level.batches.iter().zip(&work) {
            let draws = work.is_some()
                && !matches!(batch.kind, BatchKind::ClipPush(..) | BatchKind::ClipPop(_));
            content_before.push(content_before.last().copied().unwrap_or(0) + usize::from(draws));
        }

        pass.set_bind_group(0, &page.camera_bind_group, &[camera_offset(options)]);
        let mut skipped_pops = Vec::new();
        // Layers and images bind their textures to group 1 in its place.
        let mut coverage_bound = false;
        for (index, (batch, work)) in level.batches.iter().zip(&work).enumerate() {
            let Some((scissor, ranges)) = work else {
                continue;
            };
            pass.set_scissor_rect(scissor[0], scissor[1], scissor[2], scissor[3]);
            match &batch.kind {
                BatchKind::ClipPush(range, pop) => {
                    let pop = (*pop).min(level.batches.len());
                    if content_before[pop] == content_before[index + 1] {
                        skipped_pops.push(pop);
                        continue;
                    }
                    // Push tests the parent depth and raises it.
                    stats.draw_calls += 1;
                    self.draw_clip(
                        level,
                        &self.clip_push,
                        (batch.depth - 1) | options.stencil,
                        range,
                        pass,
                    );
                }
                BatchKind::ClipPop(range) => {
                    if skipped_pops.last() == Some(&index) {
                        skipped_pops.pop();
                        continue;
                    }
                    // Pop tests this depth and lowers it.
                    stats.draw_calls += 1;
                    self.draw_clip(
                        level,
                        &self.clip_pop,
                        batch.depth | options.stencil,
                        range,
                        pass,
                    );
                }
                BatchKind::Fills(_) => {
                    let (Some(vertices), Some(indices)) =
                        (&level.fill_vertices, &level.fill_indices)
                    else {
                        continue;
                    };
                    pass.set_stencil_reference(batch.depth | options.stencil);
                    pass.set_pipeline(&self.fill);
                    pass.set_vertex_buffer(0, vertices.slice(..));
                    pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                    for range in ranges.iter() {
                        stats.draw_calls += 1;
                        stats.triangles += range.len() / 3;
                        pass.draw_indexed(range.clone(), 0, 0..1);
                    }
                }
                BatchKind::Vectors(_) => {
                    let Some(segments) = &level.segments else {
                        continue;
                    };
                    pass.set_stencil_reference(batch.depth | options.stencil);
                    pass.set_pipeline(&self.vectors);
                    if !coverage_bound {
                        let coverage = level.coverage.as_ref().map(|(_, group)| group);
                        pass.set_bind_group(1, coverage.unwrap_or(&self.no_coverage), &[]);
                        coverage_bound = true;
                    }
                    pass.set_vertex_buffer(0, segments.slice(..));
                    for range in ranges.iter() {
                        stats.draw_calls += 1;
                        stats.segments += range.len();
                        pass.draw(0..4, range.clone());
                    }
                }
                BatchKind::Raster(layer) => {
                    if let Some(Some(layer)) = page.layers.get(*layer) {
                        pass.set_stencil_reference(batch.depth | options.stencil);
                        stats.draw_calls += 1;
                        self.draw_layer(layer, pass);
                        coverage_bound = false;
                    }
                }
                BatchKind::Image(image) => {
                    if let Some(Some(image)) = page.images.get(*image) {
                        pass.set_stencil_reference(batch.depth | options.stencil);
                        stats.draw_calls += 1;
                        self.draw_layer(image, pass);
                        coverage_bound = false;
                    }
                }
            }
        }
    }

    fn draw_clip(
        &self,
        level: &GpuLevel,
        pipeline: &wgpu::RenderPipeline,
        reference: u32,
        range: &Range<u32>,
        pass: &mut wgpu::RenderPass<'_>,
    ) {
        let (Some(vertices), Some(indices)) = (&level.fill_vertices, &level.fill_indices) else {
            return;
        };
        pass.set_pipeline(pipeline);
        pass.set_stencil_reference(reference);
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(range.clone(), 0, 0..1);
    }
}

/// The dynamic offset of `options`' camera slot.
fn camera_offset(options: DrawOptions) -> u32 {
    (u64::from(options.slot.min(CAMERA_SLOTS - 1)) * CAMERA_STRIDE) as u32
}

/// Storage buffers of `sizes` bytes for a level's coverage fills (paths,
/// edges, bands, band entries) and their bind group. Each holds at least
/// one of the largest element, a `CoveragePath`, as bindings must.
fn coverage_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sizes: &[usize],
) -> (Vec<wgpu::Buffer>, wgpu::BindGroup) {
    let buffers: Vec<wgpu::Buffer> = sizes
        .iter()
        .map(|&len| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pdf_gpu_coverage"),
                size: (len as u64)
                    .max(size_of::<CoveragePath>() as u64)
                    .next_multiple_of(4),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        })
        .collect();
    let entries: Vec<wgpu::BindGroupEntry> = buffers
        .iter()
        .enumerate()
        .map(|(binding, buffer)| wgpu::BindGroupEntry {
            binding: binding as u32,
            resource: buffer.as_entire_binding(),
        })
        .collect();
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("pdf_gpu_coverage"),
        layout,
        entries: &entries,
    });
    (buffers, bind_group)
}

/// Scissor rectangle and visible primitive ranges of one batch.
type BatchWork<'a> = ([u32; 4], &'a [Range<u32>]);

/// `rect` limited to `bounds`; empty (and so skipped) when they do not meet.
fn clip_to(rect: PageRect, bounds: PageRect) -> PageRect {
    [
        rect[0].max(bounds[0]),
        rect[1].max(bounds[1]),
        rect[2].min(bounds[2]),
        rect[3].min(bounds[3]),
    ]
}

/// Whether `outer` holds all of `inner`.
fn covers(outer: PageRect, inner: PageRect) -> bool {
    outer[0] <= inner[0] && outer[1] <= inner[1] && outer[2] >= inner[2] && outer[3] >= inner[3]
}

fn intersects(a: PageRect, b: PageRect) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

/// Ranges of the chunks overlapping `visible`, merging adjacent ones; a batch
/// without chunks is drawn whole.
fn visible_ranges(
    chunks: &[crate::prepare::Chunk],
    whole: &Range<u32>,
    visible: PageRect,
) -> Vec<Range<u32>> {
    if chunks.is_empty() {
        return vec![whole.clone()];
    }
    let mut ranges: Vec<Range<u32>> = Vec::new();
    for chunk in chunks {
        let [x0, y0, x1, y1] = chunk.bounds;
        if x1 < visible[0] || y1 < visible[1] || x0 > visible[2] || y0 > visible[3] {
            continue;
        }
        match ranges.last_mut() {
            Some(last) if last.end == chunk.range.start => last.end = chunk.range.end,
            _ => ranges.push(chunk.range.clone()),
        }
    }
    ranges
}

/// Staging chunk size for [`Renderer::upload`]'s own belt.
const UPLOAD_CHUNK: u64 = 4 << 20;

/// A page being uploaded a chunk at a time (see [`Renderer::begin_upload`]),
/// so a dense page never costs one long frame.
pub struct PageUpload<P> {
    page: P,
    /// The page being filled, drawable once its coarsest level is written.
    gpu: GpuPage,
    textures: Vec<wgpu::Texture>,
    writes: Vec<Write>,
    items: Vec<UploadItem>,
    /// Items before this one (the coarsest level and the images) are
    /// written before the raster layers, the rest after.
    preview_items: usize,
    /// Per level, the number of items written once it is complete.
    level_ends: Vec<usize>,
    cursor: UploadCursor,
    /// Raster layers, installed on the page as each is written.
    layers: Vec<LayerUpload>,
}

/// What one upload item fills: a level's vertex, index or segment buffer
/// (`part` 0, 1, 2), or one mip level of an image.
#[derive(Clone, Copy, Debug)]
enum Write {
    Level { level: usize, part: usize },
    Image { image: usize, mip: u32 },
}

/// Where a write goes in an upload: the coarsest level, the images, then
/// the finer levels.
fn write_order(write: Write) -> usize {
    match write {
        Write::Level { level: 0, .. } => 0,
        Write::Image { .. } => 1,
        Write::Level { level, .. } => level + 1,
    }
}

/// Orders an upload's writes: the coarsest level first, as it is the
/// smallest, then the images (and the raster layers, see
/// [`PageUpload::step`]), which together are the page, only blurrier; the
/// finer levels follow. Returns how many writes precede the raster layers
/// and, per level, how many are done once it is complete.
fn plan_writes(writes: &mut [(Write, UploadItem)], levels: usize) -> (usize, Vec<usize>) {
    writes.sort_by_key(|(write, _)| write_order(*write));
    let before = |order: usize| {
        writes
            .iter()
            .filter(|(write, _)| write_order(*write) <= order)
            .count()
    };
    let preview = before(write_order(Write::Image { image: 0, mip: 0 }));
    let level_ends = (0..levels)
        .map(|level| before(write_order(Write::Level { level, part: 0 })))
        .collect();
    (preview, level_ends)
}

/// Levels complete once `written` writes are, coarsest first.
fn written_levels(level_ends: &[usize], written: usize) -> usize {
    level_ends.iter().take_while(|&&end| end <= written).count()
}

impl<P: Borrow<PreparedPage>> PageUpload<P> {
    /// Adds a raster layer, written after the coarsest level and the
    /// images and before the finer levels.
    pub fn add_layer(&mut self, layer: LayerUpload) {
        self.layers.push(layer);
    }

    /// The page as written so far, once it can be drawn (see
    /// [`Renderer::begin_upload`]).
    pub fn page(&self) -> Option<&GpuPage> {
        (self.gpu.ready_levels > 0).then_some(&self.gpu)
    }

    /// Levels written so far, coarsest first; none until the coarsest
    /// level, the images and the raster layers are.
    pub fn ready_levels(&self) -> usize {
        self.gpu.ready_levels
    }

    /// The share of the page's levels and images written, from 0 to 1.
    pub fn progress(&self) -> f32 {
        let total: u64 = self.items.iter().map(|item| item.len).sum();
        if total == 0 {
            return 1.0;
        }
        1.0 - self.cursor.remaining(&self.items) as f32 / total as f32
    }

    /// Writes up to `budget` bytes more (at least one row or word) through
    /// `belt` and submits them; returns whether the upload is complete.
    /// Hosts keep one belt, whose staging buffers are reused once the GPU
    /// has copied from them: `Queue::write_buffer` allocates staging memory
    /// per call, which now and then costs tens of milliseconds.
    pub fn step(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        belt: &mut wgpu::util::StagingBelt,
        budget: u64,
    ) -> bool {
        submit_writes(device, queue, belt, |encoder, belt| {
            let mut left = budget;
            left = left.saturating_sub(self.write_items(encoder, belt, left, self.preview_items));
            if self.cursor.item >= self.preview_items {
                for layer in &mut self.layers {
                    if left == 0 {
                        break;
                    }
                    left = left.saturating_sub(layer.write(encoder, belt, left));
                    if layer.done() {
                        layer.install(&mut self.gpu);
                    }
                }
            }
            let layers_done = self.layers.iter().all(LayerUpload::done);
            if layers_done && left > 0 {
                self.write_items(encoder, belt, left, self.items.len());
            }
            if layers_done && self.cursor.item >= self.preview_items {
                self.gpu.ready_levels = written_levels(&self.level_ends, self.cursor.item);
            }
            layers_done && self.cursor.done(&self.items)
        })
    }

    /// Records up to `budget` bytes (at least a unit) of the items before
    /// `end`; returns the bytes.
    fn write_items(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        belt: &mut wgpu::util::StagingBelt,
        budget: u64,
        end: usize,
    ) -> u64 {
        let page = self.page.borrow();
        let mut written = 0;
        for (item, bytes) in self.cursor.next(&self.items[..end], budget) {
            written += bytes.end - bytes.start;
            let range = bytes.start as usize..bytes.end as usize;
            match self.writes[item] {
                Write::Level { level, part } => {
                    let (prepared, gpu) = (&page.levels[level], &self.gpu.levels[level]);
                    let (source, buffer): (&[u8], _) = match part {
                        0 => (
                            bytemuck::cast_slice(&prepared.fill_vertices),
                            gpu.fill_vertices.as_ref(),
                        ),
                        1 => (
                            bytemuck::cast_slice(&prepared.fill_indices),
                            gpu.fill_indices.as_ref(),
                        ),
                        2 => (
                            bytemuck::cast_slice(&prepared.segments),
                            gpu.segments.as_ref(),
                        ),
                        part => {
                            let source: &[u8] = match part {
                                3 => bytemuck::cast_slice(&prepared.coverage_paths),
                                4 => bytemuck::cast_slice(&prepared.coverage_segments),
                                5 => bytemuck::cast_slice(&prepared.bands),
                                _ => bytemuck::cast_slice(&prepared.band_entries),
                            };
                            let buffer =
                                gpu.coverage.as_ref().map(|(buffers, _)| &buffers[part - 3]);
                            (source, buffer)
                        }
                    };
                    if let (Some(buffer), Some(size)) =
                        (buffer, wgpu::BufferSize::new(range.len() as u64))
                    {
                        belt.write_buffer(encoder, buffer, bytes.start, size)
                            .copy_from_slice(&source[range]);
                    }
                }
                Write::Image { image, mip } => {
                    let (pixels, [width, _]) = image_mip(&page.images[image], mip);
                    write_rows(
                        encoder,
                        belt,
                        &self.textures[image],
                        mip,
                        pixels,
                        width,
                        bytes,
                    );
                }
            }
        }
        written
    }

    /// Bytes not written yet.
    pub fn remaining_bytes(&self) -> u64 {
        self.cursor.remaining(&self.items)
            + self
                .layers
                .iter()
                .map(|layer| layer.cursor.remaining(&layer.items()))
                .sum::<u64>()
    }
}

/// A raster layer texture written a chunk at a time (see
/// [`Renderer::begin_layer`]).
pub struct LayerUpload {
    index: usize,
    size: [u32; 2],
    rgba: Vec<u8>,
    texture: wgpu::Texture,
    /// The layer drawn from the texture, until it is installed on a page.
    layer: Option<RasterLayer>,
    cursor: UploadCursor,
}

impl LayerUpload {
    /// Puts the layer on `page`, replacing (and freeing) the one it had.
    fn install(&mut self, page: &mut GpuPage) {
        let Some(layer) = self.layer.take() else {
            return;
        };
        page.gpu_bytes += layer.bytes;
        if let Some(slot) = page.layers.get_mut(self.index)
            && let Some(old) = slot.replace(layer)
        {
            page.gpu_bytes = page.gpu_bytes.saturating_sub(old.bytes);
        }
    }

    fn items(&self) -> [UploadItem; 1] {
        let row = u64::from(self.size[0]) * 4;
        [UploadItem {
            len: row * u64::from(self.size[1]),
            unit: row,
        }]
    }

    fn done(&self) -> bool {
        self.cursor.done(&self.items())
    }

    /// Records up to `budget` more bytes (at least a row); returns the bytes.
    fn write(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        belt: &mut wgpu::util::StagingBelt,
        budget: u64,
    ) -> u64 {
        let items = self.items();
        let mut written = 0;
        for (_, bytes) in self.cursor.next(&items, budget) {
            written += bytes.end - bytes.start;
            write_rows(
                encoder,
                belt,
                &self.texture,
                0,
                &self.rgba,
                self.size[0],
                bytes,
            );
        }
        written
    }

    /// Writes up to `budget` more bytes and submits them; returns whether
    /// the layer is complete.
    pub fn step(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        belt: &mut wgpu::util::StagingBelt,
        budget: u64,
    ) -> bool {
        submit_writes(device, queue, belt, |encoder, belt| {
            self.write(encoder, belt, budget);
            self.done()
        })
    }
}

/// Records writes through `belt` in a fresh encoder and submits them.
fn submit_writes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    belt: &mut wgpu::util::StagingBelt,
    record: impl FnOnce(&mut wgpu::CommandEncoder, &mut wgpu::util::StagingBelt) -> bool,
) -> bool {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("pdf_gpu_upload"),
    });
    let done = record(&mut encoder, belt);
    belt.finish();
    queue.submit([encoder.finish()]);
    belt.recall();
    done
}

/// Copies whole rows `bytes` of a `width`-pixel RGBA8 image into `mip` of
/// `texture` through `belt`, padding rows to the copy alignment.
fn write_rows(
    encoder: &mut wgpu::CommandEncoder,
    belt: &mut wgpu::util::StagingBelt,
    texture: &wgpu::Texture,
    mip: u32,
    pixels: &[u8],
    width: u32,
    bytes: Range<u64>,
) {
    let row_bytes = width as usize * 4;
    let padded = row_bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
    let range = bytes.start as usize..bytes.end as usize;
    let (first_row, rows) = (range.start / row_bytes, range.len() / row_bytes);
    let Some(size) = wgpu::BufferSize::new((padded * rows) as u64) else {
        return;
    };
    // Texel copies start at a multiple of the texel size.
    let alignment = wgpu::BufferSize::new(4).expect("non-zero");
    let slice = belt.allocate(size, alignment);
    {
        let mut view = slice
            .get_mapped_range_mut()
            .expect("staging belt slices are mapped");
        for (row, source) in pixels[range].chunks_exact(row_bytes).enumerate() {
            view.slice(row * padded..row * padded + row_bytes)
                .copy_from_slice(source);
        }
    }
    encoder.copy_buffer_to_texture(
        wgpu::TexelCopyBufferInfo {
            buffer: slice.buffer(),
            layout: wgpu::TexelCopyBufferLayout {
                offset: slice.offset(),
                bytes_per_row: Some(padded as u32),
                rows_per_image: Some(rows as u32),
            },
        },
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: mip,
            origin: wgpu::Origin3d {
                x: 0,
                y: first_row as u32,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::Extent3d {
            width,
            height: rows as u32,
            depth_or_array_layers: 1,
        },
    );
}

/// Pixels and size of mip level `mip` of `image`.
fn image_mip(image: &PreparedImage, mip: u32) -> (&[u8], [u32; 2]) {
    match mip.checked_sub(1) {
        None => (&image.rgba.0, image.size),
        Some(index) => {
            let (pixels, size) = &image.mips[index as usize];
            (pixels, *size)
        }
    }
}

/// `len` bytes to write in whole `unit`s (a row of a texture, a word of a
/// buffer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UploadItem {
    len: u64,
    unit: u64,
}

/// Where an upload has got to: the next item and the offset in it.
#[derive(Clone, Copy, Debug, Default)]
struct UploadCursor {
    item: usize,
    offset: u64,
}

impl UploadCursor {
    /// The next chunks, in order, together at most `budget` bytes, except
    /// that at least one unit is always taken so every call progresses.
    fn next(&mut self, items: &[UploadItem], budget: u64) -> Vec<(usize, Range<u64>)> {
        let mut chunks = Vec::new();
        let mut left = budget;
        while let Some(item) = items.get(self.item) {
            let unit = item.unit.max(1);
            let mut take = (left / unit * unit).min(item.len - self.offset);
            if take == 0 && item.len > self.offset {
                if !chunks.is_empty() {
                    break;
                }
                take = unit.min(item.len - self.offset);
            }
            if take > 0 {
                chunks.push((self.item, self.offset..self.offset + take));
            }
            self.offset += take;
            left = left.saturating_sub(take);
            if self.offset >= item.len {
                self.item += 1;
                self.offset = 0;
            }
            if left == 0 {
                break;
            }
        }
        chunks
    }

    fn done(&self, items: &[UploadItem]) -> bool {
        self.item >= items.len()
    }

    fn remaining(&self, items: &[UploadItem]) -> u64 {
        items
            .iter()
            .skip(self.item)
            .map(|item| item.len)
            .sum::<u64>()
            - self.offset
    }
}

/// A stencil attachment cleared to zero for one page render pass.
pub fn stencil_attachment(view: &wgpu::TextureView) -> wgpu::RenderPassDepthStencilAttachment<'_> {
    wgpu::RenderPassDepthStencilAttachment {
        view,
        depth_ops: None,
        stencil_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(0),
            store: wgpu::StoreOp::Discard,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
        pollster::block_on(adapter.request_device(&Default::default())).ok()
    }

    /// A 64 × 16 pt page filled with `color`, on the GPU.
    fn solid_page(
        renderer: &Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color: [u8; 4],
    ) -> GpuPage {
        use glyphst_pdf::vector::{PagePath, PathVerb, VectorDraw, VectorItem, VectorPage};
        let page = VectorPage {
            bounds: [0.0, 0.0, 64.0, 16.0],
            draws: vec![VectorDraw {
                item: VectorItem::Fill {
                    path: PagePath {
                        verbs: vec![
                            PathVerb::MoveTo,
                            PathVerb::LineTo,
                            PathVerb::LineTo,
                            PathVerb::LineTo,
                            PathVerb::Close,
                        ],
                        points: vec![[0.0, 0.0], [64.0, 0.0], [64.0, 16.0], [0.0, 16.0]],
                    },
                    even_odd: false,
                    color,
                },
                clip: None,
            }],
            ..VectorPage::default()
        };
        let mut upload = renderer.begin_upload(device, PreparedPage::new(&page));
        let mut belt = wgpu::util::StagingBelt::new(device.clone(), 1 << 16);
        while !upload.step(device, queue, &mut belt, u64::MAX) {}
        renderer.finish_upload(upload)
    }

    /// Draws with `draw` into a 64 × 16 px target cleared to white and reads
    /// it back as RGBA rows.
    fn render(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        draw: impl FnOnce(&mut wgpu::RenderPass<'_>, &Camera),
    ) -> Vec<u8> {
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let target = OffscreenTarget::new(
            device,
            format,
            [64, 16],
            wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let camera = Camera::fitting([0.0, 0.0, 64.0, 16.0], [64, 16]);
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256 * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = target.begin_pass(&mut encoder, wgpu::Color::WHITE);
            draw(&mut pass, &camera);
        }
        encoder.copy_texture_to_buffer(
            target.texture().as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &read,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(16),
                },
            },
            wgpu::Extent3d {
                width: 64,
                height: 16,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        read.slice(..).get_mapped_range().unwrap().to_vec()
    }

    fn pixel(image: &[u8], x: usize, y: usize) -> [u8; 3] {
        let at = y * 256 + x * 4;
        [image[at], image[at + 1], image[at + 2]]
    }

    #[test]
    fn marked_pixels_show_only_drawings_made_for_them() {
        let Some((device, queue)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let renderer = Renderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let black = solid_page(&renderer, &device, &queue, [0, 0, 0, 255]);
        let red = solid_page(&renderer, &device, &queue, [255, 0, 0, 255]);
        // The left half, in clip space.
        let left: Vec<u8> = [
            [-1.0f32, 1.0],
            [0.0, 1.0],
            [-1.0, -1.0],
            [-1.0, -1.0],
            [0.0, 1.0],
            [0.0, -1.0],
        ]
        .iter()
        .flat_map(|[x, y]| x.to_ne_bytes().into_iter().chain(y.to_ne_bytes()))
        .collect();
        let left = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: &left,
            usage: wgpu::BufferUsages::VERTEX,
        });

        // Marked: the black page skips the left half, the red one drawn
        // for the mark fills only it.
        let image = render(&device, &queue, |pass, camera| {
            renderer.mark(pass, &left, 6, true);
            renderer.draw(&queue, &black, camera, pass);
            let options = DrawOptions {
                stencil: STENCIL_REVEAL_BIT,
                ..DrawOptions::default()
            };
            renderer.draw_with(&queue, &red, camera, options, pass);
        });
        assert_eq!(pixel(&image, 8, 8), [255, 0, 0]);
        assert_eq!(pixel(&image, 56, 8), [0, 0, 0]);

        // Cleared again, pages draw everywhere.
        let image = render(&device, &queue, |pass, camera| {
            renderer.mark(pass, &left, 6, true);
            renderer.mark(pass, &left, 6, false);
            renderer.draw(&queue, &black, camera, pass);
        });
        assert_eq!(pixel(&image, 8, 8), [0, 0, 0]);
    }

    #[test]
    fn drawings_fade_with_their_own_opacity() {
        let Some((device, queue)) = device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let renderer = Renderer::new(&device, wgpu::TextureFormat::Rgba8Unorm);
        let black = solid_page(&renderer, &device, &queue, [0, 0, 0, 255]);
        // Half the black over white: mid grey.
        let image = render(&device, &queue, |pass, camera| {
            let options = DrawOptions {
                opacity: 0.5,
                ..DrawOptions::default()
            };
            renderer.draw_with(&queue, &black, camera, options, pass);
        });
        let [r, g, b] = pixel(&image, 32, 8);
        assert!(
            (r as i32 - 128).abs() <= 2 && r == g && g == b,
            "{r} {g} {b}"
        );

        // One page twice in a frame, in two slots: each keeps its opacity.
        let image = render(&device, &queue, |pass, camera| {
            for (slot, opacity, clip) in [
                (0, 0.0, [0.0, 0.0, 32.0, 16.0]),
                (1, 1.0, [32.0, 0.0, 64.0, 16.0]),
            ] {
                let options = DrawOptions {
                    clip: Some(clip),
                    opacity,
                    slot,
                    ..DrawOptions::default()
                };
                renderer.draw_with(&queue, &black, camera, options, pass);
            }
        });
        assert_eq!(pixel(&image, 8, 8), [255, 255, 255]);
        assert_eq!(pixel(&image, 56, 8), [0, 0, 0]);
    }

    #[test]
    fn a_fitting_camera_shows_the_whole_page() {
        let camera = Camera::fitting([0.0, 0.0, 600.0, 800.0], [150, 300]);
        assert_eq!(camera.scale, 0.25);
        let [x0, y0, x1, y1] = camera.visible_page_rect(0.0);
        assert!(x0 <= 0.0 && x1 >= 600.0 && y0 <= 0.0 && y1 >= 800.0);
        assert!((y0 + y1 - 800.0).abs() < 1e-3, "centred");
    }

    #[test]
    fn a_selection_serves_small_moves_and_zooms_in() {
        // Two strokes far apart: one by the view, one 900 pt away.
        let level = GpuLevel {
            max_scale: f32::INFINITY,
            fill_vertices: None,
            fill_indices: None,
            segments: None,
            coverage: None,
            batches: vec![Batch {
                kind: BatchKind::Vectors(0..2),
                clip: None,
                depth: 0,
                chunks: vec![
                    crate::prepare::Chunk {
                        range: 0..1,
                        bounds: [90.0, 90.0, 110.0, 110.0],
                    },
                    crate::prepare::Chunk {
                        range: 1..2,
                        bounds: [1000.0, 1000.0, 1010.0, 1010.0],
                    },
                ],
            }],
        };
        let at = |focus: [f32; 2], scale: f32| Camera::centered(focus, scale, [200, 200]);
        let camera = at([100.0, 100.0], 1.0);
        let selection = Selection::new(0, &level, &camera);
        assert_eq!(selection.ranges, vec![vec![0..1]]);
        assert!(selection.serves(0, &level, &at([150.0, 120.0], 1.0)));
        assert!(selection.serves(0, &level, &at([100.0, 100.0], 2.0)));
        // Past the margin, zoomed in further, out, or at another level.
        assert!(!selection.serves(0, &level, &at([500.0, 100.0], 1.0)));
        assert!(!selection.serves(0, &level, &at([100.0, 100.0], 2.5)));
        assert!(!selection.serves(0, &level, &at([100.0, 100.0], 0.25)));
        assert!(!selection.serves(1, &level, &camera));
    }

    #[test]
    fn scissors_keep_drawing_inside_the_page() {
        // A 100×100 pt page at 2 px/pt, scrolled so its top-left corner is
        // 10 pt into a 400×400 px view.
        let camera = Camera {
            origin: [-10.0, -10.0],
            scale: 2.0,
            viewport: [400, 400],
            rotation: 0,
        };
        let page = [0.0, 0.0, 100.0, 100.0];
        assert_eq!(camera.scissor(page), Some([20, 20, 200, 200]));
        // A clip reaching past the page is cut at its edge, one beside it
        // draws nothing.
        let wide_clip = [-50.0, 50.0, 500.0, 60.0];
        assert_eq!(
            camera.scissor(clip_to(wide_clip, page)),
            Some([20, 120, 200, 20])
        );
        assert_eq!(
            camera.scissor(clip_to([120.0, 0.0, 150.0, 10.0], page)),
            None
        );
        // A drawing clipped to the lower half of the page keeps its batches
        // there too.
        let lower = clip_to(page, [0.0, 50.0, 100.0, 100.0]);
        assert_eq!(camera.scissor(lower), Some([20, 120, 200, 100]));
        assert_eq!(
            camera.scissor(clip_to(wide_clip, lower)),
            Some([20, 120, 200, 20])
        );
        assert_eq!(
            camera.scissor(clip_to([0.0, 0.0, 100.0, 40.0], lower)),
            None
        );
    }

    #[test]
    fn uploads_write_the_coarsest_level_and_images_before_finer_levels() {
        let item = UploadItem { len: 4, unit: 4 };
        let level = |level, part| (Write::Level { level, part }, item);
        let image = |image, mip| (Write::Image { image, mip }, item);
        // As `begin_upload` lists them: every level's parts, then images.
        let mut writes = vec![
            level(0, 0),
            level(0, 2),
            level(1, 0),
            level(1, 1),
            level(2, 2),
            image(0, 0),
            image(0, 1),
        ];
        let (preview, ends) = plan_writes(&mut writes, 4);
        let order: Vec<_> = writes
            .iter()
            .map(|(write, _)| match *write {
                Write::Level { level, .. } => format!("L{level}"),
                Write::Image { .. } => "I".into(),
            })
            .collect();
        assert_eq!(order, ["L0", "L0", "I", "I", "L1", "L1", "L2"]);
        // The raster layers go after the coarsest level and the images.
        assert_eq!(preview, 4);
        // Level 3 writes nothing, so it is done with level 2.
        assert_eq!(ends, [2, 6, 7, 7]);
        assert_eq!(written_levels(&ends, 0), 0);
        assert_eq!(
            written_levels(&ends, 3),
            1,
            "level 0 is done before the images"
        );
        assert_eq!(written_levels(&ends, 6), 2);
        assert_eq!(written_levels(&ends, 7), 4);
    }

    #[test]
    fn a_partly_written_page_draws_its_finest_written_level() {
        let max_scales = [0.25, 1.0, 4.0, f32::INFINITY];
        assert_eq!(drawn_level(&max_scales, 4, 2.0), 2);
        assert_eq!(drawn_level(&max_scales, 2, 2.0), 1);
        assert_eq!(drawn_level(&max_scales, 1, 64.0), 0);
        // Coarse views draw their own level whatever is written above it.
        assert_eq!(drawn_level(&max_scales, 2, 0.1), 0);
    }

    #[test]
    fn uploads_write_every_byte_once_within_the_budget() {
        let items = [
            UploadItem { len: 10, unit: 2 },
            UploadItem { len: 0, unit: 4 },
            UploadItem { len: 12, unit: 4 },
            UploadItem { len: 30, unit: 10 },
        ];
        let mut cursor = UploadCursor::default();
        let mut written = vec![Vec::new(); items.len()];
        let mut steps = 0;
        while !cursor.done(&items) {
            let before = cursor.remaining(&items);
            let chunks = cursor.next(&items, 8);
            let total: u64 = chunks
                .iter()
                .map(|(_, range)| range.end - range.start)
                .sum();
            assert!(total > 0, "every step progresses");
            // Over the budget only for a single unit larger than it.
            assert!(total <= 8 || chunks.len() == 1 && total == items[chunks[0].0].unit);
            assert_eq!(cursor.remaining(&items), before - total);
            for (item, range) in chunks {
                assert_eq!(range.start % items[item].unit, 0);
                assert_eq!((range.end - range.start) % items[item].unit, 0);
                written[item].push(range);
            }
            steps += 1;
        }
        assert!(steps > 1);
        for (item, ranges) in items.iter().zip(&written) {
            let mut end = 0;
            for range in ranges {
                assert_eq!(range.start, end, "contiguous, without overlap");
                end = range.end;
            }
            assert_eq!(end, item.len);
        }
        assert!(cursor.next(&items, 8).is_empty());
        let mut whole = UploadCursor::default();
        assert_eq!(whole.next(&items, u64::MAX).len(), 3);
        assert!(whole.done(&items));
    }

    #[test]
    fn rotated_cameras_map_page_corners_and_back() {
        let page = [10.0, 20.0, 110.0, 220.0];
        for rotation in 0..4u8 {
            let camera = Camera::centered_rotated([60.0, 120.0], 2.0, [600, 600], rotation);
            // The page's centre stays in the middle of the view.
            let center = camera.to_pixels([60.0, 120.0]);
            assert!((center[0] - 300.0).abs() < 1e-3 && (center[1] - 300.0).abs() < 1e-3);
            for corner in [[10.0, 20.0], [110.0, 20.0], [10.0, 220.0], [110.0, 220.0]] {
                let back = camera.to_page(camera.to_pixels(corner));
                assert!((back[0] - corner[0]).abs() < 1e-3 && (back[1] - corner[1]).abs() < 1e-3);
            }
            // A quarter turn puts the page on its side.
            let [x, y, width, height] = camera.scissor(page).unwrap();
            let expected = if rotation % 2 == 0 {
                [200, 400]
            } else {
                [400, 200]
            };
            assert_eq!([width, height], expected, "rotation {rotation}");
            assert_eq!([x + width / 2, y + height / 2], [300, 300]);
            // The visible page rectangle covers the whole page here.
            let visible = camera.visible_page_rect(0.0);
            assert!(visible[0] <= 10.0 && visible[1] <= 20.0);
            assert!(visible[2] >= 110.0 && visible[3] >= 220.0);
        }
        // Clockwise with y down: the top-right corner goes to the bottom
        // right, a point to the right of the origin goes below it.
        assert_eq!(rotate([1.0, 0.0], 1), [0.0, 1.0]);
        assert_eq!(rotate([0.0, 1.0], 1), [-1.0, 0.0]);
        assert_eq!(rotate([1.0, 2.0], 2), [-1.0, -2.0]);
        assert_eq!(rotate(rotate([3.0, 4.0], 1), 3), [3.0, 4.0]);
    }
}
