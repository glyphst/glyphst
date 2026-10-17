// Prelude shared by every effect. An effect file defines
//     fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32>
// returning premultiplied RGBA, and may use everything declared here.

struct EffectUniforms {
    // Target size in pixels.
    resolution: vec2<f32>,
    // Pointer position in pixels, or (-1, -1) when outside.
    pointer: vec2<f32>,
    // Seconds since the effect started.
    time: f32,
    // Transition progress from 0 (all `from`) to 1 (all `to`).
    progress: f32,
    // +1 for forward (next page, next tab), -1 for backward.
    direction: f32,
    // How opaque the result is drawn: below 1 for the background of a
    // transparent window. The prelude applies it; effects can ignore it.
    opacity: f32,
    // The effect's parameters, in manifest order.
    params: vec4<f32>,
}

@group(0) @binding(0) var<uniform> fx: EffectUniforms;
// Transitions: the outgoing and incoming views. Overlays: both are the
// view's content. Backgrounds: unused.
@group(0) @binding(1) var fx_from: texture_2d<f32>;
@group(0) @binding(2) var fx_to: texture_2d<f32>;
@group(0) @binding(3) var fx_sampler: sampler;

struct FxVertex {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn fx_vertex(@builtin(vertex_index) vertex: u32) -> FxVertex {
    // One triangle covering the viewport.
    let uv = vec2<f32>(f32((vertex << 1u) & 2u), f32(vertex & 2u));
    var out: FxVertex;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

fn fx_inside(uv: vec2<f32>) -> bool {
    return all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
}

fn fx_sample_from(uv: vec2<f32>) -> vec4<f32> {
    let color = textureSampleLevel(fx_from, fx_sampler, uv, 0.0);
    return select(vec4<f32>(0.0), color, fx_inside(uv));
}

fn fx_sample_to(uv: vec2<f32>) -> vec4<f32> {
    let color = textureSampleLevel(fx_to, fx_sampler, uv, 0.0);
    return select(vec4<f32>(0.0), color, fx_inside(uv));
}

fn fx_smoothstep01(t: f32) -> f32 {
    let x = clamp(t, 0.0, 1.0);
    return x * x * (3.0 - 2.0 * x);
}

// Backgrounds with `image: true`: the wallpaper (premultiplied, mipmapped)
// is `fx_from`; without one it is a single transparent pixel.
fn fx_image_size() -> vec2<f32> {
    return vec2<f32>(textureDimensions(fx_from));
}

// `uv` in the target to `uv` in the wallpaper, cropped to fill the target
// (as CSS `background-size: cover`).
fn fx_cover_uv(uv: vec2<f32>) -> vec2<f32> {
    let image = max(fx_image_size(), vec2<f32>(1.0));
    let target_aspect = fx.resolution.x / max(fx.resolution.y, 1.0);
    let image_aspect = image.x / image.y;
    var scale = vec2<f32>(1.0);
    if image_aspect > target_aspect {
        scale.x = target_aspect / image_aspect;
    } else {
        scale.y = image_aspect / target_aspect;
    }
    return vec2<f32>(0.5) + (uv - vec2<f32>(0.5)) * scale;
}

// The wallpaper at target `uv`, blurred by sampling mip level `lod`.
fn fx_sample_image(uv: vec2<f32>, lod: f32) -> vec4<f32> {
    return textureSampleLevel(fx_from, fx_sampler, clamp(fx_cover_uv(uv), vec2<f32>(0.0), vec2<f32>(1.0)), lod);
}

// Overlays: the colour of premultiplied `color` before premultiplying.
fn fx_unpremultiply(color: vec4<f32>) -> vec3<f32> {
    return select(vec3<f32>(0.0), color.rgb / color.a, color.a > 0.0);
}

// A `colour` parameter: sRGB packed as 0xRRGGBB in a float.
fn fx_colour(packed: f32) -> vec3<f32> {
    let value = u32(round(clamp(packed, 0.0, 16777215.0)));
    return vec3<f32>(
        f32((value >> 16u) & 255u),
        f32((value >> 8u) & 255u),
        f32(value & 255u),
    ) / 255.0;
}

fn fx_luminance(rgb: vec3<f32>) -> f32 {
    return dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment
fn fx_fragment(in: FxVertex) -> @location(0) vec4<f32> {
    return effect(in.uv, in.position.xy) * fx.opacity;
}
