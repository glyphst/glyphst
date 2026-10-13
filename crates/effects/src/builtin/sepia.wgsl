// @effect id: sepia
// @effect name: Sepia
// @effect kind: overlay
// @effect param: strength float 0 1 0.6 Strength
// Warm, paper-like pages.

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let page = fx_sample_from(uv);
    let rgb = fx_unpremultiply(page);
    let sepia = vec3<f32>(
        dot(rgb, vec3<f32>(0.393, 0.769, 0.189)),
        dot(rgb, vec3<f32>(0.349, 0.686, 0.168)),
        dot(rgb, vec3<f32>(0.272, 0.534, 0.131)),
    );
    let tinted = mix(rgb, min(sepia, vec3<f32>(1.0)), fx.params.x);
    return vec4<f32>(tinted * page.a, page.a);
}
