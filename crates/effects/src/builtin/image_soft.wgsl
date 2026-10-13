// @effect id: image-soft
// @effect name: Soft wallpaper
// @effect kind: background
// @effect image: true
// @effect param: blur float 0 8 4 Blur
// @effect param: dim float 0 1 0.35 Dim
// The wallpaper blurred and dimmed, so pages stand out; black without one.

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let image = fx_sample_image(uv, fx.params.x);
    return vec4<f32>(image.rgb * (1.0 - fx.params.y), 1.0);
}
