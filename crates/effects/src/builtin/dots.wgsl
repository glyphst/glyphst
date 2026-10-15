// @effect id: dots
// @effect name: Dots
// @effect kind: background
// @effect animated: true
// @effect param: spacing float 8 32 14 Dot spacing
// @effect param: density float 0 1 0.5 Dot density
// @effect param: glow float 0 1 0.35 Amber glow
// The glyph design's dot field: a quiet grid of dots on the base, a few
// lit in amber, drifting slowly.

fn hash(cell: vec2<f32>) -> f32 {
    return fract(sin(dot(cell, vec2<f32>(127.1, 311.7))) * 43758.5453);
}

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let base = vec3<f32>(0.055, 0.059, 0.067);
    let spacing = max(fx.params.x, 4.0);
    let cell = floor(pixel / spacing);
    let local = fract(pixel / spacing) - vec2<f32>(0.5);
    let seed = hash(cell);
    // Fewer dots at low density, never none.
    let present = step(1.0 - max(fx.params.y, 0.05), seed * 0.999);
    let radius = 1.1 / spacing;
    let dot_mask = present * (1.0 - smoothstep(radius * 0.6, radius * 1.4, length(local)));
    let lit = step(0.985, fract(seed * 7.13 + fx.time * 0.05)) * fx.params.z;
    let dot_color = mix(vec3<f32>(0.227, 0.247, 0.275), vec3<f32>(1.0, 0.69, 0.0), lit);
    return vec4<f32>(mix(base, dot_color, dot_mask), 1.0);
}
