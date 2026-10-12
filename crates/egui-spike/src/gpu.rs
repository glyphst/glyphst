//! GPU resources and the egui paint callback that draws pages.
//!
//! As in photocraft's `gpu_canvas`, resources live in egui-wgpu's
//! `callback_resources` and each frame's callback carries only plain view
//! data. `prepare` draws the visible pages with `pdf-gpu` into a
//! multisampled offscreen target (egui's own pass has no MSAA or stencil);
//! `paint` composites that image into egui's pass, blending two images
//! during a page transition.

use std::{collections::HashMap, time::Instant};

use eframe::egui_wgpu::{self, CallbackResources, CallbackTrait, ScreenDescriptor};
use glyphst_pdf::vector::LayerImage;
use glyphst_pdf_gpu::{
    Camera, DrawStats, GpuPage, OffscreenTarget, PreparedPage, Renderer, SAMPLE_COUNT,
    STENCIL_FORMAT,
};
use wgpu::util::DeviceExt as _;

/// GPU memory kept for resident pages; least recently drawn pages beyond it
/// are dropped (visible pages never are).
pub const PAGE_BUDGET_BYTES: u64 = 1 << 30;
const OVERVIEW_DIMENSION: u32 = 8192;

/// One page to draw into a layer.
#[derive(Clone, Copy, Debug)]
pub struct PageDraw {
    pub page: usize,
    pub camera: Camera,
    /// Paper rectangle in view pixels: `[x0, y0, x1, y1]`.
    pub paper: [f32; 4],
}

/// Physical pixel size egui uses for a callback in `rect`.
pub fn pixel_size(rect: egui::Rect, pixels_per_point: f32) -> [u32; 2] {
    let size = |min: f32, max: f32| {
        ((max * pixels_per_point).round() - (min * pixels_per_point).round()).max(1.0) as u32
    };
    [size(rect.min.x, rect.max.x), size(rect.min.y, rect.max.y)]
}

#[derive(Clone, Debug, Default)]
pub struct FrameStats {
    pub encode_ms: f64,
    pub draw: DrawStats,
    pub resident: usize,
    pub gpu_bytes: u64,
}

struct Resident {
    page: GpuPage,
    last_drawn: u64,
}

pub struct SpikeGpu {
    renderer: Renderer,
    format: wgpu::TextureFormat,
    pages: HashMap<usize, Resident>,
    frame: u64,
    targets: [Option<OffscreenTarget>; 2],
    paper: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    composite_layout: wgpu::BindGroupLayout,
    composite_bind_group: Option<wgpu::BindGroup>,
    mix: wgpu::Buffer,
    sampler: wgpu::Sampler,
    pub stats: FrameStats,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MixUniform {
    progress: f32,
    direction: f32,
    size: [f32; 2],
}

impl MixUniform {
    fn bytes(&self) -> [u8; 16] {
        let mut bytes = [0; 16];
        for (index, value) in [self.progress, self.direction, self.size[0], self.size[1]]
            .into_iter()
            .enumerate()
        {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_ne_bytes());
        }
        bytes
    }
}

impl SpikeGpu {
    pub fn install(render_state: &egui_wgpu::RenderState) {
        let device = &render_state.device;
        let format = render_state.target_format;
        let module = device.create_shader_module(wgpu::include_wgsl!("spike.wgsl"));

        let paper_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("spike_paper"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let paper = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("spike_paper"),
            layout: Some(&paper_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_paper"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 8,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_paper"),
                targets: &[Some(format.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            // Page passes carry a stencil attachment; paper ignores it.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: STENCIL_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: SAMPLE_COUNT,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("spike_composite"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let composite_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("spike_composite"),
                bind_group_layouts: &[Some(&composite_layout)],
                immediate_size: 0,
            });
        // egui's pass: the surface format, one sample, no depth (the
        // defaults eframe is started with).
        let composite = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("spike_composite"),
            layout: Some(&composite_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_composite"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_composite"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let mix = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("spike_mix"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("spike_composite"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let gpu = Self {
            renderer: Renderer::new(device, format),
            format,
            pages: HashMap::new(),
            frame: 0,
            targets: [None, None],
            paper,
            composite,
            composite_layout,
            composite_bind_group: None,
            mix,
            sampler,
            stats: FrameStats::default(),
        };
        render_state.renderer.write().callback_resources.insert(gpu);
    }

    pub fn is_resident(&self, page: usize) -> bool {
        self.pages.contains_key(&page)
    }

    /// Uploads a prepared page with its raster layers and, when the page is
    /// dense, its overview. Returns the time taken in milliseconds.
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        index: usize,
        prepared: &PreparedPage,
        layers: &[(usize, LayerImage)],
    ) -> f64 {
        let started = Instant::now();
        let mut page = self.renderer.upload(device, prepared);
        for (layer, image) in layers {
            self.renderer.set_raster_layer(
                device,
                queue,
                &mut page,
                *layer,
                image.rect,
                &image.rgba,
                image.size,
            );
        }
        if page.wants_overview() {
            let dimension = device
                .limits()
                .max_texture_dimension_2d
                .min(OVERVIEW_DIMENSION);
            self.renderer
                .build_overview(device, queue, &mut page, dimension);
        }
        self.pages.insert(
            index,
            Resident {
                page,
                last_drawn: self.frame,
            },
        );
        started.elapsed().as_secs_f64() * 1000.0
    }

    /// Drops least recently drawn pages outside `keep` until the resident
    /// pages fit `budget`; returns the dropped pages.
    pub fn evict(&mut self, keep: &[usize], budget: u64) -> Vec<usize> {
        let mut dropped = Vec::new();
        while self.resident_bytes() > budget {
            let Some((&page, _)) = self
                .pages
                .iter()
                .filter(|(page, _)| !keep.contains(page))
                .min_by_key(|(_, resident)| resident.last_drawn)
            else {
                break;
            };
            self.pages.remove(&page);
            dropped.push(page);
        }
        dropped
    }

    fn resident_bytes(&self) -> u64 {
        self.pages
            .values()
            .map(|resident| resident.page.gpu_bytes)
            .sum()
    }

    /// "overview" or "level N" for `page` at `scale`.
    pub fn source(&self, page: usize, scale: f32) -> Option<String> {
        let page = &self.pages.get(&page)?.page;
        Some(match page.overview_scale() {
            Some(overview) if scale <= overview => "overview".into(),
            _ => format!("level {}", page.level_for_scale(scale)),
        })
    }

    fn ensure_targets(&mut self, device: &wgpu::Device, size: [u32; 2], layers: usize) {
        for index in 0..layers.min(2) {
            if self.targets[index]
                .as_ref()
                .is_none_or(|target| target.size() != size)
            {
                self.targets[index] = Some(OffscreenTarget::new(
                    device,
                    self.format,
                    size,
                    wgpu::TextureUsages::TEXTURE_BINDING,
                ));
                self.composite_bind_group = None;
            }
        }
        if self.composite_bind_group.is_none()
            && let Some(a) = &self.targets[0]
        {
            // Without a second layer of the same size, `a` stands in for it.
            let b = self.targets[1]
                .as_ref()
                .filter(|b| b.size() == a.size())
                .unwrap_or(a);
            self.composite_bind_group =
                Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("spike_composite"),
                    layout: &self.composite_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(a.view()),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(b.view()),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: self.mix.as_entire_binding(),
                        },
                    ],
                }));
        }
    }
}

/// The per-frame paint callback: one layer normally, two during a
/// transition (outgoing, incoming).
pub struct PagesCallback {
    pub rect: egui::Rect,
    pub layers: Vec<Vec<PageDraw>>,
    /// Transition progress from layer 0 to layer 1.
    pub progress: f32,
    pub direction: f32,
}

impl CallbackTrait for PagesCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(gpu) = resources.get_mut::<SpikeGpu>() else {
            return Vec::new();
        };
        let started = Instant::now();
        let size = pixel_size(self.rect, screen.pixels_per_point);
        let layers = self.layers.len().clamp(1, 2);
        gpu.ensure_targets(device, size, layers);
        gpu.frame += 1;

        // All paper quads share one buffer: queued writes land before the
        // submission, so per-layer buffers would need one write each anyway.
        let mut quads: Vec<[f32; 2]> = Vec::new();
        let mut paper_ranges = Vec::new();
        for draws in self.layers.iter().take(2) {
            let start = quads.len() as u32;
            for draw in draws {
                let [x0, y0, x1, y1] = draw.paper;
                let clip = |x: f32, y: f32| {
                    [
                        x / size[0] as f32 * 2.0 - 1.0,
                        1.0 - y / size[1] as f32 * 2.0,
                    ]
                };
                let (a, b, c, d) = (clip(x0, y0), clip(x1, y0), clip(x0, y1), clip(x1, y1));
                quads.extend([a, b, c, c, b, d]);
            }
            paper_ranges.push(start..quads.len() as u32);
        }
        let paper = (!quads.is_empty()).then(|| {
            let bytes: Vec<u8> = quads
                .iter()
                .flat_map(|[x, y]| x.to_ne_bytes().into_iter().chain(y.to_ne_bytes()))
                .collect();
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("spike_paper"),
                contents: &bytes,
                usage: wgpu::BufferUsages::VERTEX,
            })
        });

        let mut stats = DrawStats::default();
        let mut encoder = device.create_command_encoder(&Default::default());
        for (layer, draws) in self.layers.iter().take(2).enumerate() {
            let Some(target) = &gpu.targets[layer] else {
                continue;
            };
            let mut pass = target.begin_pass(&mut encoder, wgpu::Color::TRANSPARENT);
            if let Some(paper) = &paper {
                pass.set_pipeline(&gpu.paper);
                pass.set_vertex_buffer(0, paper.slice(..));
                pass.draw(paper_ranges[layer].clone(), 0..1);
            }
            for draw in draws {
                if let Some(resident) = gpu.pages.get_mut(&draw.page) {
                    resident.last_drawn = gpu.frame;
                    let page_stats =
                        gpu.renderer
                            .draw(queue, &resident.page, &draw.camera, &mut pass);
                    stats.draw_calls += page_stats.draw_calls;
                    stats.triangles += page_stats.triangles;
                    stats.segments += page_stats.segments;
                    stats.used_overview |= page_stats.used_overview;
                }
            }
        }
        queue.write_buffer(
            &gpu.mix,
            0,
            &MixUniform {
                progress: if layers == 2 { self.progress } else { 0.0 },
                direction: self.direction,
                size: [size[0] as f32, size[1] as f32],
            }
            .bytes(),
        );
        gpu.stats = FrameStats {
            encode_ms: started.elapsed().as_secs_f64() * 1000.0,
            draw: stats,
            resident: gpu.pages.len(),
            gpu_bytes: gpu.resident_bytes(),
        };
        vec![encoder.finish()]
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        let Some(gpu) = resources.get::<SpikeGpu>() else {
            return;
        };
        let Some(bind_group) = &gpu.composite_bind_group else {
            return;
        };
        pass.set_pipeline(&gpu.composite);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_pixel_size_rounds_edges_like_egui() {
        let rect = egui::Rect::from_min_max(egui::pos2(10.3, 0.0), egui::pos2(110.6, 50.0));
        assert_eq!(pixel_size(rect, 1.5), [151, 75]);
        assert_eq!(pixel_size(egui::Rect::NOTHING, 1.0), [1, 1]);
    }
}
