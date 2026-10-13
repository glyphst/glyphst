// @effect id: image
// @effect name: Wallpaper
// @effect kind: background
// @effect image: true
// @effect param: dim float 0 1 0 Dim
// The wallpaper image, filling the window; black without one.

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let image = fx_sample_image(uv, 0.0);
    return vec4<f32>(image.rgb * (1.0 - fx.params.x), 1.0);
}
