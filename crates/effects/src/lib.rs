//! Shader effects: transitions between views, animated backgrounds and
//! overlays, defined as data in the manner of filmcraft's effects.
//!
//! An effect is a WGSL file defining
//! `fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32>` (premultiplied
//! RGBA), compiled after a shared prelude (`prelude.wgsl`) that provides the
//! uniforms (`fx.time`, `fx.progress`, `fx.params`, …) and the input images.
//! Its manifest is a block of `// @effect key: value` lines at the top of the
//! file, so built-in and user effects are described the same way:
//!
//! ```text
//! // @effect id: aurora
//! // @effect name: Aurora
//! // @effect kind: background        (transition, background or overlay)
//! // @effect animated: true          (repaints continuously)
//! // @effect duration: 300           (transitions, milliseconds)
//! // @effect image: true              (backgrounds drawing the wallpaper)
//! // @effect hidden: true             (not offered to choose)
//! // @effect param: speed float 0 3 0.4 Speed
//! ```
//!
//! User effects form their own layer over the built-ins: one may replace a
//! built-in by id, and [`EffectRegistry::reload_user`] swaps the whole layer
//! when the user's files change. A user effect that fails to parse or
//! compile is reported and skipped; the app keeps using its built-in choice.

use std::{
    fmt,
    path::{Path, PathBuf},
    time::SystemTime,
};

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt as _;

/// Draws the view's content unchanged; used when no effect applies.
pub const PRESENT: &str = "present";
pub const SLIDE_OVER: &str = "slide-over";
pub const CROSSFADE: &str = "crossfade";
pub const SOLID: &str = "solid";
pub const AURORA: &str = "aurora";
pub const IMAGE: &str = "image";
pub const IMAGE_SOFT: &str = "image-soft";
pub const IMAGE_PARALLAX: &str = "image-parallax";
pub const SEPIA: &str = "sepia";
pub const NIGHT: &str = "night";
pub const PAPER: &str = "paper";

/// Effects may declare at most this many parameters (one `vec4`).
pub const MAX_PARAMS: usize = 4;

const PRELUDE: &str = include_str!("prelude.wgsl");
const BUILTIN: [(&str, &str); 11] = [
    ("present.wgsl", include_str!("builtin/present.wgsl")),
    ("crossfade.wgsl", include_str!("builtin/crossfade.wgsl")),
    ("slide_over.wgsl", include_str!("builtin/slide_over.wgsl")),
    ("solid.wgsl", include_str!("builtin/solid.wgsl")),
    ("aurora.wgsl", include_str!("builtin/aurora.wgsl")),
    ("image.wgsl", include_str!("builtin/image.wgsl")),
    ("image_soft.wgsl", include_str!("builtin/image_soft.wgsl")),
    (
        "image_parallax.wgsl",
        include_str!("builtin/image_parallax.wgsl"),
    ),
    ("sepia.wgsl", include_str!("builtin/sepia.wgsl")),
    ("night.wgsl", include_str!("builtin/night.wgsl")),
    ("paper.wgsl", include_str!("builtin/paper.wgsl")),
];

/// A starting point for a user effect: a commented background.
pub const TEMPLATE: &str = include_str!("template.wgsl");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectKind {
    /// Blends from an outgoing view to an incoming one as progress runs
    /// from 0 to 1.
    Transition,
    /// Fills the area behind pages.
    Background,
    /// Post-processes the view's content.
    Overlay,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParamKind {
    Float { min: f32, max: f32 },
    Toggle,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParamDef {
    pub id: String,
    pub label: String,
    pub kind: ParamKind,
    pub default: f32,
}

/// An effect's manifest.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectDef {
    pub id: String,
    pub name: String,
    pub kind: EffectKind,
    /// Whether the effect changes over time, so its view must keep
    /// repainting while it shows.
    pub animated: bool,
    /// Transition length in milliseconds.
    pub duration_ms: u32,
    /// A background that draws the wallpaper image.
    pub image: bool,
    /// Used by the app itself, not offered as a choice.
    pub hidden: bool,
    pub params: Vec<ParamDef>,
    /// Where the effect came from: a built-in file name or a user path.
    pub source: String,
    /// A user effect (from the effects folder), not a built-in.
    pub user: bool,
}

impl EffectDef {
    /// Default parameter values in manifest order.
    pub fn default_params(&self) -> [f32; MAX_PARAMS] {
        let mut values = [0.0; MAX_PARAMS];
        for (value, param) in values.iter_mut().zip(&self.params) {
            *value = param.default;
        }
        values
    }

    /// `values` limited to each parameter's range (toggles to 0 or 1);
    /// values past the declared parameters become 0.
    pub fn sanitize_params(&self, values: [f32; MAX_PARAMS]) -> [f32; MAX_PARAMS] {
        let mut clean = self.default_params();
        for (index, param) in self.params.iter().enumerate() {
            let value = values[index];
            if !value.is_finite() {
                continue;
            }
            clean[index] = match param.kind {
                ParamKind::Float { min, max } => value.clamp(min, max),
                ParamKind::Toggle => {
                    if value >= 0.5 {
                        1.0
                    } else {
                        0.0
                    }
                }
            };
        }
        clean
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectError {
    pub source: String,
    pub message: String,
}

impl fmt::Display for EffectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.source, self.message)
    }
}

/// Parses the `// @effect` manifest lines of an effect file.
pub fn parse_manifest(source_name: &str, text: &str) -> Result<EffectDef, String> {
    let mut id = None;
    let mut name = None;
    let mut kind = None;
    let mut animated = false;
    let mut image = false;
    let mut hidden = false;
    let mut duration_ms = 300;
    let mut params = Vec::new();
    for line in text.lines() {
        let Some(entry) = line.trim().strip_prefix("// @effect ") else {
            continue;
        };
        let (key, value) = entry
            .split_once(':')
            .ok_or_else(|| format!("manifest line without a key: {line}"))?;
        let value = value.trim();
        match key.trim() {
            "id" => id = Some(value.to_string()),
            "name" => name = Some(value.to_string()),
            "kind" => {
                kind = Some(match value {
                    "transition" => EffectKind::Transition,
                    "background" => EffectKind::Background,
                    "overlay" => EffectKind::Overlay,
                    _ => return Err(format!("unknown kind {value:?}")),
                })
            }
            "animated" => animated = value == "true",
            "image" => image = value == "true",
            "hidden" => hidden = value == "true",
            "duration" => {
                duration_ms = value
                    .parse()
                    .map_err(|_| format!("duration must be milliseconds: {value:?}"))?
            }
            "param" => params.push(parse_param(value)?),
            other => return Err(format!("unknown manifest key {other:?}")),
        }
    }
    if params.len() > MAX_PARAMS {
        return Err(format!("at most {MAX_PARAMS} parameters"));
    }
    Ok(EffectDef {
        id: id.ok_or("missing id")?,
        name: name.ok_or("missing name")?,
        kind: kind.ok_or("missing kind")?,
        animated,
        duration_ms,
        image,
        hidden,
        params,
        source: source_name.to_string(),
        user: false,
    })
}

/// `<id> float <min> <max> <default> <label…>` or `<id> toggle <default> <label…>`.
fn parse_param(value: &str) -> Result<ParamDef, String> {
    let mut words = value.split_whitespace();
    let id = words.next().ok_or("parameter without an id")?.to_string();
    let number = |word: Option<&str>| {
        word.and_then(|word| word.parse::<f32>().ok())
            .ok_or_else(|| format!("bad number in parameter {id:?}"))
    };
    let (kind, default) = match words.next() {
        Some("float") => {
            let min = number(words.next())?;
            let max = number(words.next())?;
            let default = number(words.next())?;
            if !(min <= default && default <= max) {
                return Err(format!("default of {id:?} is outside {min}..{max}"));
            }
            (ParamKind::Float { min, max }, default)
        }
        Some("toggle") => (ParamKind::Toggle, number(words.next())?),
        other => return Err(format!("unknown parameter type {other:?} for {id:?}")),
    };
    let label = words.collect::<Vec<_>>().join(" ");
    Ok(ParamDef {
        label: if label.is_empty() { id.clone() } else { label },
        id,
        kind,
        default,
    })
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    resolution: [f32; 2],
    pointer: [f32; 2],
    time: f32,
    progress: f32,
    direction: f32,
    padding: f32,
    params: [f32; 4],
}

/// Per-draw values of an effect.
pub struct EffectInputs<'a> {
    /// The outgoing view (transitions) or the content (overlays).
    pub from: &'a wgpu::TextureView,
    /// The incoming view; overlays and backgrounds pass `from` again.
    pub to: &'a wgpu::TextureView,
    pub resolution: [f32; 2],
    pub pointer: Option<[f32; 2]>,
    pub time: f32,
    pub progress: f32,
    pub direction: f32,
    pub params: [f32; MAX_PARAMS],
}

struct CompiledEffect {
    def: EffectDef,
    pipeline: wgpu::RenderPipeline,
}

/// An effect ready to draw in a render pass this frame.
pub struct PreparedEffect {
    user: bool,
    index: usize,
    bind_group: wgpu::BindGroup,
}

pub struct EffectRegistry {
    format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    sampler: wgpu::Sampler,
    builtin: Vec<CompiledEffect>,
    user: Vec<CompiledEffect>,
    errors: Vec<EffectError>,
}

/// What the effects folder holds, to notice changes: each `*.wgsl` file in
/// name order with its modification time and size.
pub fn user_stamp(dir: &Path) -> Vec<(PathBuf, Option<SystemTime>, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut stamp: Vec<_> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "wgsl"))
        .map(|path| {
            let metadata = std::fs::metadata(&path).ok();
            let modified = metadata.as_ref().and_then(|m| m.modified().ok());
            let len = metadata.map_or(0, |m| m.len());
            (path, modified, len)
        })
        .collect();
    stamp.sort_by(|a, b| a.0.cmp(&b.0));
    stamp
}

impl EffectRegistry {
    /// Compiles the built-in effects for a single-sampled `format` target
    /// without depth, blending premultiplied colour over what is there.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effects"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture(1),
                texture(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("effects"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("effects"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            // Effects blur a mipmapped wallpaper by sampling coarser levels.
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let mut registry = Self {
            format,
            layout,
            pipeline_layout,
            sampler,
            builtin: Vec::new(),
            user: Vec::new(),
            errors: Vec::new(),
        };
        for (name, text) in BUILTIN {
            match registry.compile_effect(device, name, text) {
                Ok(effect) => registry.builtin.push(effect),
                Err(error) => registry.errors.push(error),
            }
        }
        registry
    }

    /// Replaces the user effects with every `*.wgsl` effect in `dir`, in
    /// name order, and their errors with this load's. A later file with an
    /// id already loaded replaces the earlier one.
    pub fn reload_user(&mut self, device: &wgpu::Device, dir: &Path) {
        self.user.clear();
        self.errors.clear();
        for (path, _, _) in user_stamp(dir) {
            let name = path.display().to_string();
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    self.add(device, &name, &text);
                }
                Err(error) => self.errors.push(EffectError {
                    source: name,
                    message: error.to_string(),
                }),
            }
        }
    }

    /// Parses and compiles one user effect; returns whether it was added.
    pub fn add(&mut self, device: &wgpu::Device, source_name: &str, text: &str) -> bool {
        match self.compile_effect(device, source_name, text) {
            Ok(mut effect) => {
                effect.def.user = true;
                self.user
                    .retain(|existing| existing.def.id != effect.def.id);
                self.user.push(effect);
                true
            }
            Err(error) => {
                self.errors.push(error);
                false
            }
        }
    }

    fn compile_effect(
        &self,
        device: &wgpu::Device,
        source_name: &str,
        text: &str,
    ) -> Result<CompiledEffect, EffectError> {
        parse_manifest(source_name, text)
            .and_then(|def| {
                let pipeline = self.compile(device, &def, text)?;
                Ok(CompiledEffect { def, pipeline })
            })
            .map_err(|message| EffectError {
                source: source_name.to_string(),
                message,
            })
    }

    /// The effect with `id`: a user one first, then a built-in.
    fn locate(&self, id: &str) -> Option<(bool, usize)> {
        let position = |effects: &[CompiledEffect]| effects.iter().position(|e| e.def.id == id);
        position(&self.user)
            .map(|index| (true, index))
            .or_else(|| position(&self.builtin).map(|index| (false, index)))
    }

    fn compile(
        &self,
        device: &wgpu::Device,
        def: &EffectDef,
        text: &str,
    ) -> Result<wgpu::RenderPipeline, String> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&def.id),
            source: wgpu::ShaderSource::Wgsl(format!("{PRELUDE}\n{text}").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&def.id),
            layout: Some(&self.pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("fx_vertex"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fx_fragment"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: self.format,
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
        match pollster::block_on(scope.pop()) {
            None => Ok(pipeline),
            Some(error) => Err(error.to_string()),
        }
    }

    /// Every effect in use: the built-ins in their order (a user effect with
    /// a built-in's id in its place), then the other user effects.
    pub fn defs(&self) -> impl Iterator<Item = &EffectDef> {
        let replaced = |id: &str| self.user.iter().find(|user| user.def.id == id);
        let builtin = self
            .builtin
            .iter()
            .map(move |effect| &replaced(&effect.def.id).unwrap_or(effect).def);
        let added = self.user.iter().filter(|user| {
            !self
                .builtin
                .iter()
                .any(|builtin| builtin.def.id == user.def.id)
        });
        builtin.chain(added.map(|effect| &effect.def))
    }

    pub fn find(&self, id: &str) -> Option<&EffectDef> {
        let (user, index) = self.locate(id)?;
        Some(if user {
            &self.user[index].def
        } else {
            &self.builtin[index].def
        })
    }

    /// Effects that failed to load, for display.
    pub fn errors(&self) -> &[EffectError] {
        &self.errors
    }

    /// Prepares effect `id` for this frame; `None` when it is not
    /// registered.
    pub fn prepare(
        &self,
        device: &wgpu::Device,
        id: &str,
        inputs: &EffectInputs<'_>,
    ) -> Option<PreparedEffect> {
        let (user, index) = self.locate(id)?;
        let uniforms = Uniforms {
            resolution: inputs.resolution,
            pointer: inputs.pointer.unwrap_or([-1.0, -1.0]),
            time: inputs.time,
            progress: inputs.progress.clamp(0.0, 1.0),
            direction: inputs.direction,
            padding: 0.0,
            params: inputs.params,
        };
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("effect_uniforms"),
            contents: bytemuck::bytes_of(&uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("effect"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(inputs.from),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(inputs.to),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        Some(PreparedEffect {
            user,
            index,
            bind_group,
        })
    }

    /// Draws a prepared effect over the pass's current viewport. Prepare and
    /// draw in the same frame: a reload in between may move effects.
    pub fn draw(&self, prepared: &PreparedEffect, pass: &mut wgpu::RenderPass<'_>) {
        let layer = if prepared.user {
            &self.user
        } else {
            &self.builtin
        };
        let Some(effect) = layer.get(prepared.index) else {
            return;
        };
        pass.set_pipeline(&effect.pipeline);
        pass.set_bind_group(0, &prepared.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests;
