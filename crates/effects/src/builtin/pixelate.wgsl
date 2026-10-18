// @effect id: pixelate
// @effect name: Pixelate
// @effect kind: transition
// @effect duration: 500
// @effect param: block float 4 128 48 Largest block
// The outgoing view breaks into blocks, which turn into the incoming view
// and sharpen again.

const PI: f32 = 3.14159265;

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let t = fx.progress;
    if t <= 0.0 {
        return fx_sample_from(uv);
    }
    if t >= 1.0 {
        return fx_sample_to(uv);
    }
    let eased = fx_ease_in_out_cubic(t);
    let block = mix(1.0, fx.params.x, sin(PI * eased));
    let resolution = max(fx.resolution, vec2<f32>(1.0));
    let at = clamp(
        (floor(uv * resolution / block) * block + block * 0.5) / resolution,
        vec2<f32>(0.0),
        vec2<f32>(1.0),
    );
    return mix(fx_sample_from(at), fx_sample_to(at), smoothstep(0.4, 0.6, eased));
}
