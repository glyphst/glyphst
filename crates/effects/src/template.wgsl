// A Glyphst effect: WGSL after a shared prelude. Save it in the effects
// folder and the Reader picks it up (and again whenever it changes).
//
// The manifest is the `// @effect` lines:
// @effect id: my-effect
// @effect name: My effect
// @effect kind: background
// @effect animated: true
// @effect param: speed float 0 4 1 Speed
// @effect param: glow float 0 1 0.5 Glow
//
// Kinds:
//   background  fills the window behind the pages. With `image: true` the
//               wallpaper is fx_from: sample it with fx_sample_image(uv, lod).
//   transition  runs from fx_from (outgoing) to fx_to (incoming) as
//               fx.progress goes 0 -> 1, over `duration` milliseconds.
//   overlay     tints the pages: fx_from is the page image (transparent
//               around the pages; keep its alpha).
//
// Uniforms (prelude): fx.resolution and fx.pointer (pixels; -1 outside),
// fx.time (seconds), fx.progress, fx.direction (+1 forward, -1 back) and
// fx.params (the params above, in order). Return premultiplied RGBA; the
// prelude multiplies it by a transparent window's opacity.

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let t = fx.time * fx.params.x;
    let wave = 0.5 + 0.5 * sin(uv.x * 6.0 + t) * cos(uv.y * 4.0 - t * 0.7);
    let base = mix(vec3<f32>(0.05, 0.06, 0.12), vec3<f32>(0.10, 0.12, 0.25), uv.y);
    let glow = vec3<f32>(0.3, 0.5, 0.9) * wave * fx.params.y;
    return vec4<f32>(base + glow, 1.0);
}
