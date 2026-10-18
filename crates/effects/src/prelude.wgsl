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
    // The part of fx_from and fx_to in use, from their top left, as a
    // fraction of their size (a view draws into a window-sized image).
    // fx_sample_from and fx_sample_to apply it.
    extent: vec2<f32>,
    // Transitions: where the switch came from, in pixels (the tab or card
    // picked; may lie outside the view). The centre when there is none.
    origin: vec2<f32>,
    // Transitions: a number that differs from one run to the next, for
    // patterns that should not repeat.
    seed: f32,
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
    let color = textureSampleLevel(fx_from, fx_sampler, uv * fx.extent, 0.0);
    return select(vec4<f32>(0.0), color, fx_inside(uv));
}

fn fx_sample_to(uv: vec2<f32>) -> vec4<f32> {
    let color = textureSampleLevel(fx_to, fx_sampler, uv * fx.extent, 0.0);
    return select(vec4<f32>(0.0), color, fx_inside(uv));
}

fn fx_smoothstep01(t: f32) -> f32 {
    let x = clamp(t, 0.0, 1.0);
    return x * x * (3.0 - 2.0 * x);
}

fn fx_ease_in_out_cubic(t: f32) -> f32 {
    let x = clamp(t, 0.0, 1.0);
    if x < 0.5 {
        return 4.0 * x * x * x;
    }
    let y = 2.0 - 2.0 * x;
    return 1.0 - y * y * y / 2.0;
}

fn fx_ease_out_cubic(t: f32) -> f32 {
    let y = 1.0 - clamp(t, 0.0, 1.0);
    return 1.0 - y * y * y;
}

// The view's width over its height.
fn fx_aspect() -> f32 {
    return fx.resolution.x / max(fx.resolution.y, 1.0);
}

// `uv` stretched to the view's aspect: x runs 0..fx_aspect(), y 0..1, so
// distances are round.
fn fx_square(uv: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(uv.x * fx_aspect(), uv.y);
}

// The origin as `uv`.
fn fx_origin() -> vec2<f32> {
    return fx.origin / max(fx.resolution, vec2<f32>(1.0));
}

// From `point` (in fx_square space) to the farthest corner of the view: how
// far a shape growing from `point` must reach to cover it.
fn fx_reach(point: vec2<f32>) -> f32 {
    let size = vec2<f32>(fx_aspect(), 1.0);
    let far = max(abs(point), abs(size - point));
    return length(far);
}

// A pseudo-random number in 0..1 for `p`, varied by fx.seed.
fn fx_hash21(p: vec2<f32>) -> f32 {
    var q = fract(p * vec2<f32>(123.34, 456.21) + fract(fx.seed * 0.1031));
    q += dot(q, q + 45.32);
    return fract(q.x * q.y);
}

// Smooth value noise in 0..1.
fn fx_value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = fx_hash21(i);
    let b = fx_hash21(i + vec2<f32>(1.0, 0.0));
    let c = fx_hash21(i + vec2<f32>(0.0, 1.0));
    let d = fx_hash21(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
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

// The wallpaper at target `uv`, blurred by `lod` (a mip level: each one
// doubles the radius). A coarse level magnified bilinearly shows its texels
// as blocks, so blurred samples take a cubic B-spline over the two levels
// about `lod`: smooth at any radius, for eight taps.
fn fx_sample_image(uv: vec2<f32>, lod: f32) -> vec4<f32> {
    let at = clamp(fx_cover_uv(uv), vec2<f32>(0.0), vec2<f32>(1.0));
    let last = f32(textureNumLevels(fx_from) - 1u);
    let level = clamp(lod, 0.0, last);
    if level < 0.5 {
        return textureSampleLevel(fx_from, fx_sampler, at, level);
    }
    let low = floor(level);
    let high = min(low + 1.0, last);
    return mix(fx_sample_bspline(at, low), fx_sample_bspline(at, high), level - low);
}

// `fx_from` at mip `level`, filtered with a cubic B-spline built from four
// bilinear taps (Sigg and Hadwiger, GPU Gems 2, ch. 20).
fn fx_sample_bspline(uv: vec2<f32>, level: f32) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(fx_from, u32(level)));
    let st = uv * size - vec2<f32>(0.5);
    let i = floor(st);
    let f = st - i;
    let f2 = f * f;
    let f3 = f2 * f;
    let w0 = (1.0 - 3.0 * f + 3.0 * f2 - f3) / 6.0;
    let w1 = (4.0 - 6.0 * f2 + 3.0 * f3) / 6.0;
    let w2 = (1.0 + 3.0 * f + 3.0 * f2 - 3.0 * f3) / 6.0;
    let w3 = f3 / 6.0;
    // Each tap blends two texels, landing between them by their weights.
    let s0 = w0 + w1;
    let s1 = w2 + w3;
    let t0 = (i - vec2<f32>(0.5) + w1 / s0) / size;
    let t1 = (i + vec2<f32>(1.5) + w3 / s1) / size;
    let a = textureSampleLevel(fx_from, fx_sampler, vec2<f32>(t0.x, t0.y), level);
    let b = textureSampleLevel(fx_from, fx_sampler, vec2<f32>(t1.x, t0.y), level);
    let c = textureSampleLevel(fx_from, fx_sampler, vec2<f32>(t0.x, t1.y), level);
    let d = textureSampleLevel(fx_from, fx_sampler, vec2<f32>(t1.x, t1.y), level);
    return s0.y * (s0.x * a + s1.x * b) + s1.y * (s0.x * c + s1.x * d);
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
