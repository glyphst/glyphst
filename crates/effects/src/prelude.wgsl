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
    padding: f32,
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

@fragment
fn fx_fragment(in: FxVertex) -> @location(0) vec4<f32> {
    return effect(in.uv, in.position.xy);
}
