// @effect id: image
// @effect name: Wallpaper
// @effect kind: background
// @effect image: true
// @effect param: dim float 0 1 0 Dim
// @effect param: blur float 0 8 0 Blur
// @effect param: parallax toggle 0 Parallax
// @effect param: depth float 0 0.1 0.03 Depth
// The wallpaper filling the window, darkened by dim and softened by blur;
// with parallax it drifts a little against the pointer. Black without one.
// (Dim comes first, where it was before blur and parallax joined it.)

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    var at = uv;
    if fx.params.z >= 0.5 {
        let depth = fx.params.w;
        var shift = vec2<f32>(0.0);
        if fx.pointer.x >= 0.0 {
            shift = (fx.pointer / max(fx.resolution, vec2<f32>(1.0)) - vec2<f32>(0.5)) * depth;
        }
        // Zoomed in by the depth on each side, so the drift never shows an edge.
        at = vec2<f32>(0.5) + (uv - vec2<f32>(0.5)) / (1.0 + 2.0 * depth) - shift;
    }
    let image = fx_sample_image(at, fx.params.y);
    return vec4<f32>(image.rgb * (1.0 - fx.params.x), 1.0);
}
