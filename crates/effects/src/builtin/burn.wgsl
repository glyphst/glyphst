// @effect id: burn
// @effect name: Burn
// @effect kind: transition
// @effect duration: 700
// @effect param: glow colour #FFB000 Edge colour
// @effect param: edge float 0.01 0.2 0.06 Edge width
// The outgoing view burns away in a noisy front with a glowing edge,
// uncovering the incoming one.

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let t = fx.progress;
    if t <= 0.0 {
        return fx_sample_from(uv);
    }
    if t >= 1.0 {
        return fx_sample_to(uv);
    }
    let eased = fx_ease_in_out_cubic(t);
    let p = fx_square(uv);
    let noise = fx_value_noise(p * 9.0) * 0.7 + fx_value_noise(p * 31.0) * 0.3;
    let edge = fx.params.y;
    let cut = eased * (1.0 + 2.0 * edge) - edge;
    let outgoing = fx_sample_from(uv);
    let incoming = fx_sample_to(uv);
    let old = smoothstep(cut - edge, cut + edge, noise);
    var color = mix(incoming, outgoing, old);
    let burning = smoothstep(cut - edge, cut, noise) * (1.0 - smoothstep(cut, cut + edge, noise));
    let glow = fx_colour(fx.params.x) * burning * 1.1 * max(outgoing.a, incoming.a);
    color = vec4<f32>(color.rgb + glow, color.a);
    return color;
}
