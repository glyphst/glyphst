// @effect id: zoom
// @effect name: Zoom
// @effect kind: transition
// @effect duration: 450
// @effect param: amount float 0 0.5 0.15 Zoom
// The outgoing view grows and fades while the incoming one settles into
// place from a little larger; going back, both move the other way.

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let t = fx.progress;
    if t <= 0.0 {
        return fx_sample_from(uv);
    }
    if t >= 1.0 {
        return fx_sample_to(uv);
    }
    let eased = fx_ease_in_out_cubic(t);
    let amount = fx.params.x * sign(fx.direction);
    let centre = vec2<f32>(0.5);
    let from_uv = (uv - centre) / (1.0 + amount * eased) + centre;
    let to_uv = (uv - centre) / (1.0 + amount * (1.0 - eased)) + centre;
    return mix(fx_sample_from(from_uv), fx_sample_to(to_uv), eased);
}
