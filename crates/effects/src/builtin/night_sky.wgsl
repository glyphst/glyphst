// @effect id: night-sky
// @effect name: Night
// @effect kind: background
// @effect param: depth float 0 1 0.6 Depth
// A deep blue glow fading to black at the edges.

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let aspect = fx.resolution.x / max(fx.resolution.y, 1.0);
    let centred = (uv - vec2<f32>(0.5)) * vec2<f32>(aspect, 1.0);
    let falloff = clamp(length(centred) * (0.9 + fx.params.x), 0.0, 1.0);
    let glow = vec3<f32>(0.11, 0.133, 0.2);
    let edge = vec3<f32>(0.031, 0.035, 0.043);
    return vec4<f32>(mix(glow, edge, falloff), 1.0);
}
