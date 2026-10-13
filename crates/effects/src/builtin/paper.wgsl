// @effect id: paper
// @effect name: Paper grain
// @effect kind: overlay
// @effect param: amount float 0 0.2 0.06 Grain
// A still, fine grain over the pages, like uncoated paper.

fn grain(pixel: vec2<f32>) -> f32 {
    let p = floor(pixel);
    return fract(sin(dot(p, vec2<f32>(12.9898, 78.233))) * 43758.5453);
}

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let page = fx_sample_from(uv);
    let rgb = fx_unpremultiply(page);
    let shade = 1.0 - fx.params.x * grain(pixel);
    let warm = rgb * vec3<f32>(1.0, 0.985, 0.95) * shade;
    return vec4<f32>(warm * page.a, page.a);
}
