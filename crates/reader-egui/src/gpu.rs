//! GPU resources and the egui paint callbacks that draw the reading stage.
//!
//! As in photocraft's `gpu_canvas`, resources live in egui-wgpu's
//! `callback_resources` and each frame's callbacks carry only plain view
//! data. A frame of the stage is two callbacks, with egui's own shapes
//! (page shadows) between them:
//!
//! 1. [`BackdropCallback`]: a background effect fills the whole stage, the
//!    window below the top bar, so it runs on behind floating panels.
//! 2. A [`ViewCallback`] per pane: `prepare` draws paper and resident pages
//!    with `pdf-gpu` into the tab's own multisampled [`OffscreenTarget`]
//!    (egui's own pass has no MSAA or stencil), then a content effect
//!    composites the page image over the backdrop: the plain "present"
//!    effect, or a transition from a snapshot of the previous view (another
//!    tab's last image, or the page before a turn) to the new image.

use std::{
    collections::{HashMap, VecDeque},
    time::Instant,
};

use eframe::egui_wgpu::{self, CallbackResources, CallbackTrait, ScreenDescriptor};
use glyphst_effects::{EffectInputs, EffectRegistry, PRESENT, PreparedEffect, SOLID};
use glyphst_pdf::vector::LayerImage;
use glyphst_pdf_gpu::{
    Camera, DrawStats, GpuPage, LayerUpload, OffscreenTarget, OverviewJob, PageUpload,
    PreparedPage, Renderer, SAMPLE_COUNT, STENCIL_FORMAT,
};
use wgpu::util::DeviceExt as _;

/// GPU memory kept for resident pages; least recently drawn pages beyond it
/// are dropped (visible pages never are).
pub const PAGE_BUDGET_BYTES: u64 = 1 << 30;
const OVERVIEW_DIMENSION: u32 = 8192;
/// Bytes of page geometry and images written to the GPU per frame, so a
/// dense page loads over a few frames instead of in one long one.
pub const UPLOAD_BYTES_PER_FRAME: u64 = 4 << 20;

/// A page of a document tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PageKey {
    pub tab: u64,
    pub page: usize,
}

/// One visible page of a view.
#[derive(Clone, Copy, Debug)]
pub struct PageDraw {
    pub page: usize,
    pub camera: Camera,
    /// Paper rectangle in view pixels: `[x0, y0, x1, y1]`.
    pub paper: [f32; 4],
}

/// An effect and its parameter values.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectUse {
    pub id: String,
    pub params: [f32; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransitionUse {
    pub effect: EffectUse,
    pub progress: f32,
    pub direction: f32,
    /// On a transition's first frame, the tab whose last drawn image becomes
    /// the outgoing view; later frames keep that snapshot.
    pub capture_from: Option<u64>,
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

/// Something being written to the GPU a chunk per frame: a page (with its
/// first raster layers), or a raster layer re-rasterized for the view.
enum Upload {
    Page {
        key: PageKey,
        upload: PageUpload<Box<PreparedPage>>,
        /// Milliseconds spent on it so far.
        ms: f64,
    },
    Layer {
        key: PageKey,
        upload: LayerUpload,
    },
}

impl Upload {
    fn key(&self) -> PageKey {
        match self {
            Self::Page { key, .. } | Self::Layer { key, .. } => *key,
        }
    }
}

/// A tab's view: the target its pages are drawn into, the outgoing image
/// of a transition, and this frame's prepared effects.
struct ViewState {
    target: OffscreenTarget,
    snapshot: Option<Snapshot>,
    prepared: Vec<PreparedEffect>,
    last_drawn: u64,
}

/// Frames a view is kept after it was last drawn, for a transition from it.
const VIEW_KEEP_FRAMES: u64 = 240;

struct Snapshot {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: [u32; 2],
}

pub struct ReaderGpu {
    renderer: Renderer,
    format: wgpu::TextureFormat,
    effects: EffectRegistry,
    pages: HashMap<PageKey, Resident>,
    /// Overviews being built, one tile per frame (see `step_overview`).
    overviews: HashMap<PageKey, OverviewJob>,
    /// Uploads in order; the first is written a chunk per frame.
    uploads: VecDeque<Upload>,
    /// Staging memory for uploads, reused from frame to frame.
    belt: wgpu::util::StagingBelt,
    frame: u64,
    /// Each shown tab's view, by tab id.
    views: HashMap<u64, ViewState>,
    paper: wgpu::RenderPipeline,
    paper_bind_group: wgpu::BindGroup,
    paper_color: wgpu::Buffer,
    /// A 1×1 texture for the backdrop's effect inputs; backgrounds don't
    /// sample them.
    blank: wgpu::TextureView,
    backdrop: Option<PreparedEffect>,
    pub stats: FrameStats,
}

impl ReaderGpu {
    pub fn install(render_state: &egui_wgpu::RenderState, user_effects: Option<&std::path::Path>) {
        let device = &render_state.device;
        let format = render_state.target_format;
        let module = device.create_shader_module(wgpu::include_wgsl!("paper.wgsl"));
        let paper_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("paper"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let paper_color = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("paper"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let paper_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("paper"),
            layout: &paper_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: paper_color.as_entire_binding(),
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("paper"),
            bind_group_layouts: &[Some(&paper_layout)],
            immediate_size: 0,
        });
        let paper = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("paper"),
            layout: Some(&pipeline_layout),
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
        let mut effects = EffectRegistry::new(device, format);
        if let Some(dir) = user_effects {
            effects.load_dir(device, dir);
        }
        let blank = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("backdrop_input"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        let gpu = Self {
            renderer: Renderer::new(device, format),
            format,
            effects,
            pages: HashMap::new(),
            overviews: HashMap::new(),
            uploads: VecDeque::new(),
            belt: wgpu::util::StagingBelt::new(device.clone(), UPLOAD_BYTES_PER_FRAME),
            frame: 0,
            views: HashMap::new(),
            paper,
            paper_bind_group,
            paper_color,
            blank,
            backdrop: None,
            stats: FrameStats::default(),
        };
        render_state.renderer.write().callback_resources.insert(gpu);
    }

    pub fn effects(&self) -> &EffectRegistry {
        &self.effects
    }

    /// Whether `key` is on the GPU or being written to it.
    pub fn is_resident(&self, key: PageKey) -> bool {
        self.pages.contains_key(&key)
            || self
                .uploads
                .iter()
                .any(|upload| matches!(upload, Upload::Page { key: k, .. } if *k == key))
    }

    /// Whether `key` can be drawn.
    pub fn is_drawable(&self, key: PageKey) -> bool {
        self.pages.contains_key(&key)
    }

    /// Whether a page is being uploaded.
    pub fn is_uploading(&self) -> bool {
        self.uploads
            .iter()
            .any(|upload| matches!(upload, Upload::Page { .. }))
    }

    /// Starts writing a prepared page and its raster layers to the GPU;
    /// [`ReaderGpu::step_upload`] continues it. A page already resident
    /// (another version of it) stays drawn until this one is complete.
    pub fn begin_upload(
        &mut self,
        device: &wgpu::Device,
        key: PageKey,
        prepared: Box<PreparedPage>,
        layers: Vec<(usize, LayerImage)>,
    ) {
        let started = Instant::now();
        let mut upload = self.renderer.begin_upload(device, prepared);
        for (index, image) in layers {
            upload.add_layer(
                self.renderer
                    .begin_layer(device, index, image.rect, image.rgba, image.size),
            );
        }
        self.uploads.push_back(Upload::Page {
            key,
            upload,
            ms: started.elapsed().as_secs_f64() * 1000.0,
        });
    }

    /// Replaces raster layer `layer` of a resident page with a texture
    /// rasterized for the current view, once it is written.
    pub fn replace_layer(
        &mut self,
        device: &wgpu::Device,
        key: PageKey,
        layer: usize,
        image: LayerImage,
    ) {
        if self.pages.contains_key(&key) {
            let upload = self
                .renderer
                .begin_layer(device, layer, image.rect, image.rgba, image.size);
            self.uploads.push_back(Upload::Layer { key, upload });
        }
    }

    /// Writes up to `budget` more bytes of the first upload. A completed
    /// page replaces any older drawing of it and starts its overview when
    /// it is dense; then the milliseconds spent on it are returned.
    pub fn step_upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        budget: u64,
    ) -> Option<f64> {
        let started = Instant::now();
        let done = match self.uploads.front_mut()? {
            Upload::Page { upload, ms, .. } => {
                let done = upload.step(device, queue, &mut self.belt, budget);
                *ms += started.elapsed().as_secs_f64() * 1000.0;
                done
            }
            Upload::Layer { upload, .. } => upload.step(device, queue, &mut self.belt, budget),
        };
        if !done {
            return None;
        }
        match self.uploads.pop_front()? {
            Upload::Layer { key, upload } => {
                if let Some(resident) = self.pages.get_mut(&key) {
                    self.renderer
                        .finish_layer(device, &mut resident.page, upload);
                }
                None
            }
            Upload::Page { key, upload, ms } => {
                let started = Instant::now();
                // A page replaced by another drawing (its translation) drops
                // the overview being built for the old one.
                self.overviews.remove(&key);
                let page = self.renderer.finish_upload(device, upload);
                if page.wants_overview() {
                    let dimension = device
                        .limits()
                        .max_texture_dimension_2d
                        .min(OVERVIEW_DIMENSION);
                    if let Some(job) = self.renderer.begin_overview(device, &page, dimension) {
                        self.overviews.insert(key, job);
                    }
                }
                self.pages.insert(
                    key,
                    Resident {
                        page,
                        last_drawn: self.frame,
                    },
                );
                Some(ms + started.elapsed().as_secs_f64() * 1000.0)
            }
        }
    }

    /// Renders one more tile of a pending overview, so building one never
    /// costs a long frame. Until it is done the page draws from vectors.
    pub fn step_overview(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let Some(&key) = self.overviews.keys().next() else {
            return;
        };
        let (Some(job), Some(resident)) = (self.overviews.get_mut(&key), self.pages.get_mut(&key))
        else {
            self.overviews.remove(&key);
            return;
        };
        if self
            .renderer
            .continue_overview(device, queue, &mut resident.page, job, 1)
        {
            self.overviews.remove(&key);
        }
    }

    /// Whether an overview or an upload is under way, a step per frame.
    pub fn has_pending_work(&self) -> bool {
        !self.overviews.is_empty() || !self.uploads.is_empty()
    }

    /// Drops least recently drawn pages outside `keep` until the resident
    /// pages fit `budget`; returns the dropped pages.
    pub fn evict(&mut self, keep: &[PageKey], budget: u64) -> Vec<PageKey> {
        let mut dropped = Vec::new();
        while self.resident_bytes() > budget {
            let Some((&key, _)) = self
                .pages
                .iter()
                .filter(|(key, _)| !keep.contains(key))
                .min_by_key(|(_, resident)| resident.last_drawn)
            else {
                break;
            };
            self.pages.remove(&key);
            self.overviews.remove(&key);
            dropped.push(key);
        }
        dropped
    }

    /// Drops every page of a closed tab.
    pub fn drop_tab(&mut self, tab: u64) {
        self.pages.retain(|key, _| key.tab != tab);
        self.overviews.retain(|key, _| key.tab != tab);
        self.uploads.retain(|upload| upload.key().tab != tab);
        self.views.remove(&tab);
    }

    fn resident_bytes(&self) -> u64 {
        self.pages
            .values()
            .map(|resident| resident.page.gpu_bytes)
            .sum()
    }

    /// "overview" or "level N" for a page at `scale`.
    pub fn source(&self, key: PageKey, scale: f32) -> Option<String> {
        let page = &self.pages.get(&key)?.page;
        Some(match page.overview_scale() {
            Some(overview) if scale <= overview => "overview".into(),
            _ => format!("level {}", page.level_for_scale(scale)),
        })
    }

    /// The view of `tab` with a target of `size`, made or remade.
    fn view(&mut self, device: &wgpu::Device, tab: u64, size: [u32; 2]) -> &mut ViewState {
        let format = self.format;
        let view = self.views.entry(tab).or_insert_with(|| ViewState {
            target: OffscreenTarget::new(device, format, size, VIEW_TARGET_USAGE),
            snapshot: None,
            prepared: Vec::new(),
            last_drawn: 0,
        });
        if view.target.size() != size {
            view.target = OffscreenTarget::new(device, format, size, VIEW_TARGET_USAGE);
        }
        view
    }
}

const VIEW_TARGET_USAGE: wgpu::TextureUsages =
    wgpu::TextureUsages::TEXTURE_BINDING.union(wgpu::TextureUsages::COPY_SRC);

/// A texture for a transition's outgoing image.
fn snapshot(device: &wgpu::Device, format: wgpu::TextureFormat, size: [u32; 2]) -> Snapshot {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("view_snapshot"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    Snapshot {
        texture,
        view,
        size,
    }
}

/// The per-frame paint callback of a document view.
pub struct ViewCallback {
    pub rect: egui::Rect,
    pub tab: u64,
    /// Every visible page; pages not resident yet get paper only.
    pub draws: Vec<PageDraw>,
    pub page_color: [f32; 4],
    pub transition: Option<TransitionUse>,
    /// Seconds for animated effects.
    pub time: f32,
    pub pointer: Option<[f32; 2]>,
}

impl ViewCallback {
    fn paper_vertices(&self, size: [u32; 2]) -> Vec<u8> {
        let clip = |x: f32, y: f32| {
            [
                x / size[0] as f32 * 2.0 - 1.0,
                1.0 - y / size[1] as f32 * 2.0,
            ]
        };
        self.draws
            .iter()
            .flat_map(|draw| {
                let [x0, y0, x1, y1] = draw.paper;
                let (a, b, c, d) = (clip(x0, y0), clip(x1, y0), clip(x0, y1), clip(x1, y1));
                [a, b, c, c, b, d]
            })
            .flat_map(|[x, y]| x.to_ne_bytes().into_iter().chain(y.to_ne_bytes()))
            .collect()
    }
}

impl CallbackTrait for ViewCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(gpu) = resources.get_mut::<ReaderGpu>() else {
            return Vec::new();
        };
        let started = Instant::now();
        let size = pixel_size(self.rect, screen.pixels_per_point);
        let frame = gpu.frame;
        let mut encoder = device.create_command_encoder(&Default::default());

        // A transition's first frame keeps the outgoing view (another tab's
        // last image, or this view's before a turn) before the new one
        // replaces it; without one of this size the outgoing view is empty.
        let capture = self.transition.as_ref().and_then(|t| t.capture_from);
        let outgoing = capture
            .and_then(|from| gpu.views.get(&from))
            .filter(|view| view.target.size() == size)
            .map(|view| view.target.texture().clone());
        let format = gpu.format;
        let view = gpu.view(device, self.tab, size);
        view.last_drawn = frame;
        if capture.is_some() {
            if view
                .snapshot
                .as_ref()
                .is_none_or(|snapshot| snapshot.size != size)
            {
                view.snapshot = Some(snapshot(device, format, size));
            }
            let snapshot = view.snapshot.as_ref().expect("made above");
            match outgoing {
                Some(texture) => encoder.copy_texture_to_texture(
                    texture.as_image_copy(),
                    snapshot.texture.as_image_copy(),
                    wgpu::Extent3d {
                        width: size[0],
                        height: size[1],
                        depth_or_array_layers: 1,
                    },
                ),
                None => {
                    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("clear_snapshot"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &snapshot.view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                }
            }
        }

        queue.write_buffer(
            &gpu.paper_color,
            0,
            bytemuck_color(self.page_color).as_slice(),
        );
        let paper = self.paper_vertices(size);
        let paper = (!paper.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("paper"),
                contents: &paper,
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        let mut stats = DrawStats::default();
        let ReaderGpu {
            views,
            pages,
            renderer,
            effects,
            paper: paper_pipeline,
            paper_bind_group,
            ..
        } = gpu;
        let Some(view) = views.get_mut(&self.tab) else {
            return Vec::new();
        };
        {
            let mut pass = view
                .target
                .begin_pass(&mut encoder, wgpu::Color::TRANSPARENT);
            if let Some(paper) = &paper {
                pass.set_pipeline(paper_pipeline);
                pass.set_bind_group(0, &*paper_bind_group, &[]);
                pass.set_vertex_buffer(0, paper.slice(..));
                pass.draw(0..(self.draws.len() * 6) as u32, 0..1);
            }
            for draw in &self.draws {
                let key = PageKey {
                    tab: self.tab,
                    page: draw.page,
                };
                if let Some(resident) = pages.get_mut(&key) {
                    resident.last_drawn = frame;
                    let page_stats = renderer.draw(queue, &resident.page, &draw.camera, &mut pass);
                    stats.draw_calls += page_stats.draw_calls;
                    stats.triangles += page_stats.triangles;
                    stats.segments += page_stats.segments;
                    stats.used_overview |= page_stats.used_overview;
                }
            }
        }

        let resolution = [size[0] as f32, size[1] as f32];
        let target = &view.target;
        let prepare = |id: &str,
                       fallback: &str,
                       from: &wgpu::TextureView,
                       params: [f32; 4],
                       progress: f32,
                       direction: f32| {
            let effect_inputs = EffectInputs {
                from,
                to: target.view(),
                resolution,
                pointer: self.pointer,
                time: self.time,
                progress,
                direction,
                params,
            };
            effects
                .prepare(device, id, &effect_inputs)
                .or_else(|| effects.prepare(device, fallback, &effect_inputs))
        };
        let content = match (&self.transition, &view.snapshot) {
            (Some(transition), Some(snapshot)) if snapshot.size == size => prepare(
                &transition.effect.id,
                PRESENT,
                &snapshot.view,
                transition.effect.params,
                transition.progress,
                transition.direction,
            ),
            _ => prepare(PRESENT, PRESENT, target.view(), [0.0; 4], 0.0, 1.0),
        };
        view.prepared = content.into_iter().collect();
        // Every pane adds to the frame's statistics.
        gpu.stats.encode_ms += started.elapsed().as_secs_f64() * 1000.0;
        gpu.stats.draw.draw_calls += stats.draw_calls;
        gpu.stats.draw.triangles += stats.triangles;
        gpu.stats.draw.segments += stats.segments;
        gpu.stats.draw.used_overview |= stats.used_overview;
        gpu.stats.resident = gpu.pages.len();
        gpu.stats.gpu_bytes = gpu.resident_bytes();
        vec![encoder.finish()]
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        if let Some(gpu) = resources.get::<ReaderGpu>()
            && let Some(view) = gpu.views.get(&self.tab)
        {
            for prepared in &view.prepared {
                gpu.effects.draw(prepared, pass);
            }
        }
    }
}

/// The per-frame paint callback of the stage's background.
pub struct BackdropCallback {
    pub rect: egui::Rect,
    pub background: EffectUse,
    /// Seconds for animated effects.
    pub time: f32,
    /// The pointer in stage pixels.
    pub pointer: Option<[f32; 2]>,
}

impl CallbackTrait for BackdropCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(gpu) = resources.get_mut::<ReaderGpu>() else {
            return Vec::new();
        };
        // The backdrop is the stage's first callback: a new frame for the
        // pages' and views' last use, and for the statistics.
        gpu.frame += 1;
        let frame = gpu.frame;
        gpu.views
            .retain(|_, view| frame - view.last_drawn <= VIEW_KEEP_FRAMES);
        gpu.stats = FrameStats::default();
        let size = pixel_size(self.rect, screen.pixels_per_point);
        let inputs = EffectInputs {
            from: &gpu.blank,
            to: &gpu.blank,
            resolution: [size[0] as f32, size[1] as f32],
            pointer: self.pointer,
            time: self.time,
            progress: 0.0,
            direction: 1.0,
            params: self.background.params,
        };
        gpu.backdrop = gpu
            .effects
            .prepare(device, &self.background.id, &inputs)
            .or_else(|| gpu.effects.prepare(device, SOLID, &inputs));
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        if let Some(gpu) = resources.get::<ReaderGpu>()
            && let Some(backdrop) = &gpu.backdrop
        {
            gpu.effects.draw(backdrop, pass);
        }
    }
}

fn bytemuck_color(color: [f32; 4]) -> Vec<u8> {
    color
        .iter()
        .flat_map(|channel| channel.to_ne_bytes())
        .collect()
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
