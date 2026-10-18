// @effect id: shrink
// @effect name: Shrink
// @effect kind: overlay
// @effect hidden: true
// fx_from drawn smaller with a box filter: four bilinear taps cover a 4×4
// texel box when the target is a quarter of the source's size, so text
// does not alias. For the book switcher's cards.

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(fx_from));
    let at = uv * fx.extent;
    // Source texels per target pixel, a quarter of it each way.
    let step = size * fx.extent / max(fx.resolution, vec2<f32>(1.0));
    let offset = step * 0.25 / size;
    let a = textureSampleLevel(fx_from, fx_sampler, at + vec2<f32>(-offset.x, -offset.y), 0.0);
    let b = textureSampleLevel(fx_from, fx_sampler, at + vec2<f32>(offset.x, -offset.y), 0.0);
    let c = textureSampleLevel(fx_from, fx_sampler, at + vec2<f32>(-offset.x, offset.y), 0.0);
    let d = textureSampleLevel(fx_from, fx_sampler, at + vec2<f32>(offset.x, offset.y), 0.0);
    return (a + b + c + d) * 0.25;
}
