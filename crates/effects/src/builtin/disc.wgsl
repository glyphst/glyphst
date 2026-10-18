// @effect id: disc
// @effect name: Disc
// @effect kind: transition
// @effect duration: 600
// @effect param: softness float 0 1 0.25 Edge softness
// The incoming view grows in a circle from where the switch came from (the
// tab or card picked); going back, the outgoing view shrinks into it.

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let t = fx.progress;
    if t <= 0.0 {
        return fx_sample_from(uv);
    }
    if t >= 1.0 {
        return fx_sample_to(uv);
    }
    let eased = fx_ease_in_out_cubic(t);
    let centre = fx_square(fx_origin());
    let distance_out = distance(fx_square(uv), centre);
    let soft = mix(0.001, 0.5, fx.params.x * fx.params.x);
    let reach = fx_reach(centre) + 2.0 * soft;
    let outgoing = fx_sample_from(uv);
    let incoming = fx_sample_to(uv);
    if fx.direction < 0.0 {
        let radius = (1.0 - eased) * reach - soft;
        return mix(outgoing, incoming, smoothstep(radius - soft, radius + soft, distance_out));
    }
    let radius = eased * reach - soft;
    return mix(incoming, outgoing, smoothstep(radius - soft, radius + soft, distance_out));
}
