// Page-space vector rendering. Geometry never changes with zoom; the camera
// maps page points to device pixels: pixel = (page - origin) * scale.

struct Camera {
    origin: vec2<f32>,
    scale: f32,
    _padding: f32,
    viewport: vec2<f32>,
    _padding2: vec2<f32>,
}

@group(0) @binding(0) var<uniform> camera: Camera;

fn to_pixels(page: vec2<f32>) -> vec2<f32> {
    return (page - camera.origin) * camera.scale;
}

fn to_clip(pixels: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(
        pixels.x / camera.viewport.x * 2.0 - 1.0,
        1.0 - pixels.y / camera.viewport.y * 2.0,
        0.0,
        1.0,
    );
}

fn premultiply(color: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(color.rgb * color.a, color.a);
}

// Fills: triangles tessellated in page space, antialiased by MSAA.

struct FillOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_fill(@location(0) position: vec2<f32>, @location(1) color: vec4<f32>) -> FillOut {
    var out: FillOut;
    out.position = to_clip(to_pixels(position));
    out.color = premultiply(color);
    return out;
}

@fragment
fn fs_fill(in: FillOut) -> @location(0) vec4<f32> {
    return in.color;
}

// Strokes: one instanced quad per segment with analytic capsule coverage.
// Round joins come from overlapping capsules; butt ends are cut flat.

const BUTT_START: u32 = 1u;
const BUTT_END: u32 = 2u;
const AA_MARGIN: f32 = 0.5;

struct StrokeOut {
    @builtin(position) position: vec4<f32>,
    // x: distance along the segment, y: signed distance across it (pixels).
    @location(0) local: vec2<f32>,
    @location(1) @interpolate(flat) length_half_width: vec2<f32>,
    @location(2) @interpolate(flat) flags: u32,
    @location(3) color: vec4<f32>,
}

@vertex
fn vs_stroke(
    @builtin(vertex_index) vertex: u32,
    @location(0) p0: vec2<f32>,
    @location(1) p1: vec2<f32>,
    @location(2) half_width: f32,
    @location(3) color: vec4<f32>,
    @location(4) flags: u32,
) -> StrokeOut {
    let s0 = to_pixels(p0);
    let s1 = to_pixels(p1);
    let delta = s1 - s0;
    let segment_length = length(delta);
    var direction = vec2<f32>(1.0, 0.0);
    if segment_length > 1e-6 {
        direction = delta / segment_length;
    }
    let normal = vec2<f32>(-direction.y, direction.x);
    // MuPDF's rule: lines under 0.1 px (including zero-width hairlines) draw
    // one pixel wide; other sub-pixel lines draw one pixel wide with their
    // coverage, as antialiasing at their real width would.
    let true_width = 2.0 * half_width * camera.scale;
    var half_width_px = 0.5 * true_width;
    var thin = 1.0;
    if true_width < 0.1 {
        half_width_px = 0.5;
    } else if true_width < 1.0 {
        half_width_px = 0.5;
        thin = true_width;
    }
    let extent = half_width_px + AA_MARGIN;

    var along = -extent;
    if (vertex & 1u) == 1u {
        along = segment_length + extent;
    }
    var across = -extent;
    if (vertex & 2u) == 2u {
        across = extent;
    }

    var out: StrokeOut;
    out.position = to_clip(s0 + direction * along + normal * across);
    out.local = vec2<f32>(along, across);
    out.length_half_width = vec2<f32>(segment_length, half_width_px);
    out.flags = flags;
    out.color = premultiply(color) * thin;
    return out;
}

@fragment
fn fs_stroke(in: StrokeOut) -> @location(0) vec4<f32> {
    let t = in.local.x;
    let d = in.local.y;
    let segment_length = in.length_half_width.x;
    let half_width = in.length_half_width.y;
    var distance = abs(d);
    var cap = 1.0;
    if t < 0.0 {
        if (in.flags & BUTT_START) != 0u {
            cap = clamp(t + 0.5, 0.0, 1.0);
        } else {
            distance = length(vec2<f32>(t, d));
        }
    } else if t > segment_length {
        if (in.flags & BUTT_END) != 0u {
            cap = clamp(segment_length - t + 0.5, 0.0, 1.0);
        } else {
            distance = length(vec2<f32>(t - segment_length, d));
        }
    }
    let coverage = clamp(half_width + 0.5 - distance, 0.0, 1.0) * cap;
    if coverage <= 0.0 {
        discard;
    }
    return in.color * coverage;
}

// Raster layers: premultiplied MuPDF pixmaps on a page-space quad.

@group(1) @binding(0) var layer_texture: texture_2d<f32>;
@group(1) @binding(1) var layer_sampler: sampler;

struct LayerOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_layer(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>) -> LayerOut {
    var out: LayerOut;
    out.position = to_clip(to_pixels(position));
    out.uv = uv;
    return out;
}

@fragment
fn fs_layer(in: LayerOut) -> @location(0) vec4<f32> {
    return textureSample(layer_texture, layer_sampler, in.uv);
}
