// Paper: white page rectangles under the page vectors, in clip space.

@vertex
fn vs_paper(@location(0) position: vec2<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(position, 0.0, 1.0);
}

@fragment
fn fs_paper() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}

// Composite: draws the offscreen page image `a` into egui's pass. During a
// transition `b` is the incoming page: it slides over `a` from the side
// with a soft shadow on its leading edge while `a` dims.
struct Mix {
    // 0 shows a, 1 shows b.
    progress: f32,
    // +1 when b comes from the right, -1 from the left.
    direction: f32,
    size: vec2<f32>,
}

@group(0) @binding(0) var image_a: texture_2d<f32>;
@group(0) @binding(1) var image_b: texture_2d<f32>;
@group(0) @binding(2) var image_sampler: sampler;
@group(0) @binding(3) var<uniform> mix_state: Mix;

struct CompositeOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_composite(@builtin(vertex_index) vertex: u32) -> CompositeOut {
    // One triangle covering the viewport egui set to the callback rect.
    let uv = vec2<f32>(f32((vertex << 1u) & 2u), f32(vertex & 2u));
    var out: CompositeOut;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

fn sample_inside(image: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    let color = textureSampleLevel(image, image_sampler, uv, 0.0);
    let inside = all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
    return select(vec4<f32>(0.0), color, inside);
}

// Width of the shadow ahead of the incoming page, in view widths.
const SHADOW_WIDTH: f32 = 0.02;

@fragment
fn fs_composite(in: CompositeOut) -> @location(0) vec4<f32> {
    let t = mix_state.progress;
    if t <= 0.0 {
        return sample_inside(image_a, in.uv);
    }
    let eased = t * t * (3.0 - 2.0 * t);
    // How far b still is from its resting place, in view widths.
    let offset = mix_state.direction * (1.0 - eased);
    let b_uv = in.uv - vec2<f32>(offset, 0.0);
    let b = sample_inside(image_b, b_uv);
    var a = sample_inside(image_a, in.uv);
    a = vec4<f32>(a.rgb * (1.0 - 0.35 * eased), a.a);
    // Shadow ahead of b's paper: b's coverage a little further along its
    // direction of travel, averaged over a few taps for a soft edge.
    var cover = 0.0;
    for (var tap = 1; tap <= 4; tap++) {
        let along = f32(tap) * SHADOW_WIDTH / 4.0 * mix_state.direction;
        cover += sample_inside(image_b, b_uv + vec2<f32>(along, 0.0)).a;
    }
    let shadow = 0.35 * cover / 4.0 * (1.0 - b.a);
    // a over a black shadow, both darkened by it.
    let shaded = vec4<f32>(a.rgb * (1.0 - shadow), a.a + shadow * (1.0 - a.a));
    return b + shaded * (1.0 - b.a);
}
