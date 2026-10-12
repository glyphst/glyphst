// @effect id: crossfade
// @effect name: Crossfade
// @effect kind: transition
// @effect duration: 250

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let t = fx_smoothstep01(fx.progress);
    return mix(fx_sample_from(uv), fx_sample_to(uv), t);
}
