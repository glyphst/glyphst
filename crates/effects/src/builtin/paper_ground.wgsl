// @effect id: paper-ground
// @effect name: Paper
// @effect kind: background
// @effect param: warmth float 0 1 0.5 Warmth
// A warm, dim paper ground: a soft diagonal gradient.

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let t = clamp((uv.x + uv.y) * 0.5, 0.0, 1.0);
    let warm = fx.params.x;
    let top = mix(vec3<f32>(0.14, 0.14, 0.14), vec3<f32>(0.165, 0.153, 0.137), warm);
    let bottom = mix(vec3<f32>(0.09, 0.09, 0.09), vec3<f32>(0.102, 0.094, 0.086), warm);
    return vec4<f32>(mix(top, bottom, t), 1.0);
}
