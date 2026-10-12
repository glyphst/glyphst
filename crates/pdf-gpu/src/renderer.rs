use std::ops::Range;

use bytemuck::{Pod, Zeroable};
use glyphst_pdf::vector::PageRect;
use wgpu::util::DeviceExt as _;

use crate::prepare::{Batch, BatchKind, FillVertex, PreparedLevel, PreparedPage, StrokeSegment};

pub const SAMPLE_COUNT: u32 = 4;

/// Render passes drawing pages need a stencil attachment of this format with
/// [`SAMPLE_COUNT`] samples, cleared to zero; non-rectangular clips use it.
pub const STENCIL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Stencil8;

/// Tile size used when rendering the overview texture.
const OVERVIEW_TILE: u32 = 2048;

/// Pages with more triangles plus segments than this are expensive to draw
/// zoomed out, where all of them land in few pixels; they get an overview.
const OVERVIEW_MIN_PRIMITIVES: usize = 100_000;

/// Maps page space to device pixels: `pixel = (page - origin) * scale`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub origin: [f32; 2],
    pub scale: f32,
    /// Viewport size in device pixels.
    pub viewport: [u32; 2],
}

impl Camera {
    /// Centers `focus` (page space) in the viewport at `scale`.
    pub fn centered(focus: [f32; 2], scale: f32, viewport: [u32; 2]) -> Self {
        Self {
            origin: [
                focus[0] - viewport[0] as f32 / 2.0 / scale,
                focus[1] - viewport[1] as f32 / 2.0 / scale,
            ],
            scale,
            viewport,
        }
    }

    /// The page-space rectangle visible in the viewport, widened by `margin`
    /// device pixels to cover hairlines widened in the shader.
    fn visible_page_rect(&self, margin: f32) -> PageRect {
        let margin = margin / self.scale;
        [
            self.origin[0] - margin,
            self.origin[1] - margin,
            self.origin[0] + self.viewport[0] as f32 / self.scale + margin,
            self.origin[1] + self.viewport[1] as f32 / self.scale + margin,
        ]
    }

    /// Page-space rectangle to a pixel scissor clamped to the viewport.
    fn scissor(&self, rect: PageRect) -> Option<[u32; 4]> {
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
    _padding: f32,
    viewport: [f32; 2],
    _padding2: [f32; 2],
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
    sampler: wgpu::Sampler,
    mip_sampler: wgpu::Sampler,
    fill: wgpu::RenderPipeline,
    stroke: wgpu::RenderPipeline,
    layer: wgpu::RenderPipeline,
    clip_push: wgpu::RenderPipeline,
    clip_pop: wgpu::RenderPipeline,
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
    overview: Option<Overview>,
    primitives: usize,
    pub gpu_bytes: u64,
}

impl GpuPage {
    /// Largest camera scale the overview texture serves, if built.
    pub fn overview_scale(&self) -> Option<f32> {
        self.overview.as_ref().map(|overview| overview.scale)
    }

    /// Whether zoomed-out views of this page are dense enough to need an
    /// overview texture.
    pub fn wants_overview(&self) -> bool {
        self.primitives >= OVERVIEW_MIN_PRIMITIVES
    }

    /// Index of the level drawn at `scale` device pixels per page point.
    pub fn level_for_scale(&self, scale: f32) -> usize {
        PreparedPage::level_for_scale(&self.max_scales, scale)
    }
}

const PREMULTIPLIED: wgpu::BlendState = wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING;

impl Renderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::include_wgsl!("shaders.wgsl"));
        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pdf_gpu_camera"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
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

        let pipeline = |name: &str,
                        vs: &str,
                        fs: &str,
                        layouts: &[Option<&wgpu::BindGroupLayout>],
                        buffers: &[wgpu::VertexBufferLayout],
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
            &[wgpu::VertexBufferLayout {
                array_stride: size_of::<FillVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Unorm8x4],
            }],
            wgpu::PrimitiveTopology::TriangleList,
            stencil_state(wgpu::StencilOperation::Keep, 0),
            wgpu::ColorWrites::ALL,
        );
        let stroke = pipeline(
            "pdf_gpu_stroke",
            "vs_stroke",
            "fs_stroke",
            &[Some(&camera_layout)],
            &[wgpu::VertexBufferLayout {
                array_stride: size_of::<StrokeSegment>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![
                    0 => Float32x2,
                    1 => Float32x2,
                    2 => Float32,
                    3 => Unorm8x4,
                    4 => Uint32,
                ],
            }],
            wgpu::PrimitiveTopology::TriangleStrip,
            stencil_state(wgpu::StencilOperation::Keep, 0),
            wgpu::ColorWrites::ALL,
        );
        let layer = pipeline(
            "pdf_gpu_layer",
            "vs_layer",
            "fs_layer",
            &[Some(&camera_layout), Some(&layer_layout)],
            &[wgpu::VertexBufferLayout {
                array_stride: size_of::<LayerVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
            }],
            wgpu::PrimitiveTopology::TriangleStrip,
            stencil_state(wgpu::StencilOperation::Keep, 0),
            wgpu::ColorWrites::ALL,
        );

        let fill_layout = [wgpu::VertexBufferLayout {
            array_stride: size_of::<FillVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Unorm8x4],
        }];
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

        Self {
            format,
            camera_layout,
            layer_layout,
            sampler,
            mip_sampler,
            fill,
            stroke,
            layer,
            clip_push,
            clip_pop,
        }
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    pub fn upload(&self, device: &wgpu::Device, page: &PreparedPage) -> GpuPage {
        let levels: Vec<GpuLevel> = page
            .levels
            .iter()
            .map(|level| upload_level(device, level))
            .collect();
        let camera = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pdf_gpu_camera"),
            size: size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pdf_gpu_camera"),
            layout: &self.camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera.as_entire_binding(),
            }],
        });
        let gpu_bytes = levels
            .iter()
            .flat_map(|level| [&level.fill_vertices, &level.fill_indices, &level.segments])
            .flatten()
            .map(wgpu::Buffer::size)
            .sum();
        GpuPage {
            bounds: page.bounds,
            camera,
            camera_bind_group,
            max_scales: levels.iter().map(|level| level.max_scale).collect(),
            levels,
            layers: page.raster_layers.iter().map(|_| None).collect(),
            overview: None,
            primitives: page.exact().fill_indices.len() / 3 + page.exact().segments.len(),
            gpu_bytes,
        }
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
        let layer = self.raster_layer(device, &texture, &self.sampler, rect);
        page.gpu_bytes += u64::from(size[0]) * u64::from(size[1]) * 4;
        page.layers[index] = Some(layer);
    }

    fn raster_layer(
        &self,
        device: &wgpu::Device,
        texture: &wgpu::Texture,
        sampler: &wgpu::Sampler,
        rect: PageRect,
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
        let [x0, y0, x1, y1] = rect;
        let vertices = [
            LayerVertex {
                position: [x0, y0],
                uv: [0.0, 0.0],
            },
            LayerVertex {
                position: [x1, y0],
                uv: [1.0, 0.0],
            },
            LayerVertex {
                position: [x0, y1],
                uv: [0.0, 1.0],
            },
            LayerVertex {
                position: [x1, y1],
                uv: [1.0, 1.0],
            },
        ];
        let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pdf_gpu_raster_layer"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        RasterLayer {
            vertices,
            bind_group,
            rect,
        }
    }

    /// Renders the page from its vectors (and raster layers) into a
    /// mipmapped texture whose longer side is `max_dimension` pixels. Every
    /// mip level is rendered from the vectors rather than downsampled.
    pub fn build_overview(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        page: &mut GpuPage,
        max_dimension: u32,
    ) {
        let [x0, y0, x1, y1] = page.bounds;
        let (width_pt, height_pt) = (x1 - x0, y1 - y0);
        if width_pt <= 0.0 || height_pt <= 0.0 {
            return;
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
        let tile_texture = |samples, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("pdf_gpu_overview_tile"),
                size: wgpu::Extent3d {
                    width: OVERVIEW_TILE,
                    height: OVERVIEW_TILE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format: self.format,
                usage,
                view_formats: &[],
            })
        };
        let msaa = tile_texture(SAMPLE_COUNT, wgpu::TextureUsages::RENDER_ATTACHMENT)
            .create_view(&Default::default());
        let resolve = tile_texture(
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let resolve_view = resolve.create_view(&Default::default());
        let stencil = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("pdf_gpu_overview_stencil"),
                size: wgpu::Extent3d {
                    width: OVERVIEW_TILE,
                    height: OVERVIEW_TILE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: SAMPLE_COUNT,
                dimension: wgpu::TextureDimension::D2,
                format: STENCIL_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default());

        for mip in 0..mip_level_count {
            let mip_width = (width >> mip).max(1);
            let mip_height = (height >> mip).max(1);
            let mip_scale = scale / (1u32 << mip) as f32;
            for tile_y in (0..mip_height).step_by(OVERVIEW_TILE as usize) {
                for tile_x in (0..mip_width).step_by(OVERVIEW_TILE as usize) {
                    let camera = Camera {
                        origin: [
                            x0 + tile_x as f32 / mip_scale,
                            y0 + tile_y as f32 / mip_scale,
                        ],
                        scale: mip_scale,
                        viewport: [OVERVIEW_TILE, OVERVIEW_TILE],
                    };
                    let mut encoder = device.create_command_encoder(&Default::default());
                    {
                        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: Some("pdf_gpu_overview"),
                            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                view: &msaa,
                                resolve_target: Some(&resolve_view),
                                ops: wgpu::Operations {
                                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                    store: wgpu::StoreOp::Discard,
                                },
                                depth_slice: None,
                            })],
                            depth_stencil_attachment: Some(stencil_attachment(&stencil)),
                            timestamp_writes: None,
                            occlusion_query_set: None,
                            multiview_mask: None,
                        });
                        self.draw_vectors(
                            queue,
                            page,
                            &camera,
                            &mut pass,
                            &mut DrawStats::default(),
                        );
                    }
                    encoder.copy_texture_to_texture(
                        resolve.as_image_copy(),
                        wgpu::TexelCopyTextureInfo {
                            texture: &texture,
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
                    // The camera uniform is rewritten per tile, so each tile
                    // is submitted before the next write.
                    queue.submit([encoder.finish()]);
                }
            }
        }

        let rect = [
            x0,
            y0,
            x0 + width as f32 / scale,
            y0 + height as f32 / scale,
        ];
        let layer = self.raster_layer(device, &texture, &self.mip_sampler, rect);
        page.gpu_bytes += u64::from(width) * u64::from(height) * 4 * 4 / 3;
        page.overview = Some(Overview { layer, scale });

        // Levels serving only scales the overview covers are never drawn.
        for level in &mut page.levels {
            if level.max_scale <= scale {
                for buffer in [
                    level.fill_vertices.take(),
                    level.fill_indices.take(),
                    level.segments.take(),
                ]
                .into_iter()
                .flatten()
                {
                    page.gpu_bytes -= buffer.size();
                    buffer.destroy();
                }
                level.batches.clear();
            }
        }
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
        let mut stats = DrawStats::default();
        match &page.overview {
            Some(overview) if camera.scale <= overview.scale => {
                self.write_camera(queue, page, camera);
                stats.used_overview = true;
                if camera.scissor(page.bounds).is_some() {
                    pass.set_bind_group(0, &page.camera_bind_group, &[]);
                    pass.set_scissor_rect(0, 0, camera.viewport[0], camera.viewport[1]);
                    pass.set_stencil_reference(0);
                    self.draw_layer(&overview.layer, pass);
                    stats.draw_calls += 1;
                }
            }
            _ => self.draw_vectors(queue, page, camera, pass, &mut stats),
        }
        stats
    }

    fn draw_layer(&self, layer: &RasterLayer, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.layer);
        pass.set_bind_group(1, &layer.bind_group, &[]);
        pass.set_vertex_buffer(0, layer.vertices.slice(..));
        pass.draw(0..4, 0..1);
    }

    fn write_camera(&self, queue: &wgpu::Queue, page: &GpuPage, camera: &Camera) {
        queue.write_buffer(
            &page.camera,
            0,
            bytemuck::bytes_of(&CameraUniform {
                origin: camera.origin,
                scale: camera.scale,
                _padding: 0.0,
                viewport: [camera.viewport[0] as f32, camera.viewport[1] as f32],
                _padding2: [0.0; 2],
            }),
        );
    }

    fn draw_vectors(
        &self,
        queue: &wgpu::Queue,
        page: &GpuPage,
        camera: &Camera,
        pass: &mut wgpu::RenderPass<'_>,
        stats: &mut DrawStats,
    ) {
        self.write_camera(queue, page, camera);
        if camera.scissor(page.bounds).is_none() {
            return;
        }
        let level = &page.levels[page.level_for_scale(camera.scale)];
        let visible = camera.visible_page_rect(2.0);
        let full = [0, 0, camera.viewport[0], camera.viewport[1]];

        // Visible work per batch, then a running count of batches that draw
        // content, so a clip whose contents are all off screen is skipped.
        let work: Vec<Option<BatchWork>> = level
            .batches
            .iter()
            .map(|batch| {
                let scissor = match batch.clip {
                    Some(clip) => camera.scissor(clip)?,
                    None => full,
                };
                let ranges = match &batch.kind {
                    BatchKind::Fills(range) | BatchKind::Strokes(range) => {
                        visible_ranges(&batch.chunks, range, visible)
                    }
                    BatchKind::ClipPush(range, _) | BatchKind::ClipPop(range) => {
                        visible_ranges(&batch.chunks, range, visible)
                    }
                    BatchKind::Raster(index) => match page.layers.get(*index) {
                        Some(Some(layer)) if intersects(layer.rect, visible) => {
                            std::iter::once(0..1).collect()
                        }
                        _ => Vec::new(),
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

        pass.set_bind_group(0, &page.camera_bind_group, &[]);
        let mut skipped_pops = Vec::new();
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
                    self.draw_clip(level, &self.clip_push, batch.depth - 1, range, pass);
                }
                BatchKind::ClipPop(range) => {
                    if skipped_pops.last() == Some(&index) {
                        skipped_pops.pop();
                        continue;
                    }
                    // Pop tests this depth and lowers it.
                    stats.draw_calls += 1;
                    self.draw_clip(level, &self.clip_pop, batch.depth, range, pass);
                }
                BatchKind::Fills(_) => {
                    let (Some(vertices), Some(indices)) =
                        (&level.fill_vertices, &level.fill_indices)
                    else {
                        continue;
                    };
                    pass.set_stencil_reference(batch.depth);
                    pass.set_pipeline(&self.fill);
                    pass.set_vertex_buffer(0, vertices.slice(..));
                    pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                    for range in ranges {
                        stats.draw_calls += 1;
                        stats.triangles += range.len() / 3;
                        pass.draw_indexed(range.clone(), 0, 0..1);
                    }
                }
                BatchKind::Strokes(_) => {
                    let Some(segments) = &level.segments else {
                        continue;
                    };
                    pass.set_stencil_reference(batch.depth);
                    pass.set_pipeline(&self.stroke);
                    pass.set_vertex_buffer(0, segments.slice(..));
                    for range in ranges {
                        stats.draw_calls += 1;
                        stats.segments += range.len();
                        pass.draw(0..4, range.clone());
                    }
                }
                BatchKind::Raster(layer) => {
                    if let Some(Some(layer)) = page.layers.get(*layer) {
                        pass.set_stencil_reference(batch.depth);
                        stats.draw_calls += 1;
                        self.draw_layer(layer, pass);
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

/// Scissor rectangle and visible primitive ranges of one batch.
type BatchWork = ([u32; 4], Vec<Range<u32>>);

fn intersects(a: PageRect, b: PageRect) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

fn upload_level(device: &wgpu::Device, level: &PreparedLevel) -> GpuLevel {
    let buffer = |label: &str, contents: &[u8], usage: wgpu::BufferUsages| {
        (!contents.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage,
            })
        })
    };
    GpuLevel {
        max_scale: level.max_scale,
        fill_vertices: buffer(
            "pdf_gpu_fill_vertices",
            bytemuck::cast_slice(&level.fill_vertices),
            wgpu::BufferUsages::VERTEX,
        ),
        fill_indices: buffer(
            "pdf_gpu_fill_indices",
            bytemuck::cast_slice(&level.fill_indices),
            wgpu::BufferUsages::INDEX,
        ),
        segments: buffer(
            "pdf_gpu_segments",
            bytemuck::cast_slice(&level.segments),
            wgpu::BufferUsages::VERTEX,
        ),
        batches: level.batches.clone(),
    }
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
