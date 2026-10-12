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
//! // @effect param: speed float 0 3 0.4 Speed
//! ```
//!
//! A user effect that fails to parse or compile is reported and skipped;
//! the app keeps using its built-in choice.

use std::{
    fmt,
    path::{Path, PathBuf},
};

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt as _;

/// Draws the view's content unchanged; used when no effect applies.
pub const PRESENT: &str = "present";
pub const SLIDE_OVER: &str = "slide-over";
pub const CROSSFADE: &str = "crossfade";
pub const SOLID: &str = "solid";
pub const AURORA: &str = "aurora";

/// Effects may declare at most this many parameters (one `vec4`).
pub const MAX_PARAMS: usize = 4;

const PRELUDE: &str = include_str!("prelude.wgsl");
const BUILTIN: [(&str, &str); 5] = [
    ("present.wgsl", include_str!("builtin/present.wgsl")),
    ("crossfade.wgsl", include_str!("builtin/crossfade.wgsl")),
    ("slide_over.wgsl", include_str!("builtin/slide_over.wgsl")),
    ("solid.wgsl", include_str!("builtin/solid.wgsl")),
    ("aurora.wgsl", include_str!("builtin/aurora.wgsl")),
];

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
    pub params: Vec<ParamDef>,
    /// Where the effect came from: a built-in file name or a user path.
    pub source: String,
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
        params,
        source: source_name.to_string(),
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
    index: usize,
    bind_group: wgpu::BindGroup,
}

pub struct EffectRegistry {
    format: wgpu::TextureFormat,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    sampler: wgpu::Sampler,
    effects: Vec<CompiledEffect>,
    errors: Vec<EffectError>,
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
            ..Default::default()
        });
        let mut registry = Self {
            format,
            layout,
            pipeline_layout,
            sampler,
            effects: Vec::new(),
            errors: Vec::new(),
        };
        for (name, text) in BUILTIN {
            registry.add(device, name, text);
        }
        registry
    }

    /// Adds every `*.wgsl` effect in `dir`, in name order. Effects that fail
    /// are recorded in [`EffectRegistry::errors`]; one whose id is already
    /// registered replaces the earlier one.
    pub fn load_dir(&mut self, device: &wgpu::Device, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "wgsl"))
            .collect();
        paths.sort();
        for path in paths {
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

    /// Parses and compiles one effect; returns whether it was added.
    pub fn add(&mut self, device: &wgpu::Device, source_name: &str, text: &str) -> bool {
        let result = parse_manifest(source_name, text).and_then(|def| {
            let pipeline = self.compile(device, &def, text)?;
            Ok(CompiledEffect { def, pipeline })
        });
        match result {
            Ok(effect) => {
                self.effects
                    .retain(|existing| existing.def.id != effect.def.id);
                self.effects.push(effect);
                true
            }
            Err(message) => {
                self.errors.push(EffectError {
                    source: source_name.to_string(),
                    message,
                });
                false
            }
        }
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

    pub fn defs(&self) -> impl Iterator<Item = &EffectDef> {
        self.effects.iter().map(|effect| &effect.def)
    }

    pub fn find(&self, id: &str) -> Option<&EffectDef> {
        self.defs().find(|def| def.id == id)
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
        let index = self.effects.iter().position(|effect| effect.def.id == id)?;
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
        Some(PreparedEffect { index, bind_group })
    }

    /// Draws a prepared effect over the pass's current viewport.
    pub fn draw(&self, prepared: &PreparedEffect, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.effects[prepared.index].pipeline);
        pass.set_bind_group(0, &prepared.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests;
