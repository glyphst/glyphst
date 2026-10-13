// @effect id: image-parallax
// @effect name: Parallax wallpaper
// @effect kind: background
// @effect image: true
// @effect param: depth float 0 0.1 0.03 Depth
// @effect param: blur float 0 8 1 Blur
// The wallpaper drifting a little against the pointer; black without one.

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    var shift = vec2<f32>(0.0);
    if fx.pointer.x >= 0.0 {
        shift = (fx.pointer / max(fx.resolution, vec2<f32>(1.0)) - vec2<f32>(0.5)) * fx.params.x;
    }
    // Zoomed in by the depth on each side, so the drift never shows an edge.
    let zoomed = vec2<f32>(0.5) + (uv - vec2<f32>(0.5)) / (1.0 + 2.0 * fx.params.x) - shift;
    let image = fx_sample_image(zoomed, fx.params.y);
    return vec4<f32>(image.rgb, 1.0);
}
