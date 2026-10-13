// @effect id: night
// @effect name: Night
// @effect kind: overlay
// @effect param: strength float 0 1 1 Strength
// Dark pages with light text: lightness inverted, hues kept (inverting and
// turning the hue half way round, as CSS does).

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let page = fx_sample_from(uv);
    let rgb = fx_unpremultiply(page);
    let inverted = vec3<f32>(1.0) - rgb;
    let turned = clamp(vec3<f32>(
        dot(inverted, vec3<f32>(-0.574, 1.430, 0.144)),
        dot(inverted, vec3<f32>(0.426, 0.430, 0.144)),
        dot(inverted, vec3<f32>(0.426, 1.430, -0.856)),
    ), vec3<f32>(0.0), vec3<f32>(1.0));
    // Off-black paper and off-white ink are easier on the eyes.
    let night = vec3<f32>(0.08, 0.08, 0.09) + turned * 0.80;
    let shown = mix(rgb, night, fx.params.x);
    return vec4<f32>(shown * page.a, page.a);
}
