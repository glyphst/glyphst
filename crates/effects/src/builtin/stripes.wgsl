// @effect id: stripes
// @effect name: Stripes
// @effect kind: transition
// @effect duration: 600
// @effect random: count angle
// @effect param: count float 2 32 10 Stripes
// @effect param: angle float 0 180 30 Angle
// @effect param: softness float 0 1 0.3 Edge softness
// Stripes at an angle close in from alternate sides; those farther from the
// middle start later.

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let t = fx.progress;
    if t <= 0.0 {
        return fx_sample_from(uv);
    }
    if t >= 1.0 {
        return fx_sample_to(uv);
    }
    let eased = fx_ease_in_out_cubic(t);
    let aspect = fx_aspect();
    let angle = radians(fx.params.y);
    let c = cos(angle);
    let s = sin(angle);
    let p = fx_square(uv);
    let along = p.x * c + p.y * s;
    let across = -p.x * s + p.y * c;
    let width = (aspect * abs(c) + abs(s)) / max(fx.params.x, 1.0);
    let position = along / width;
    let odd = (i32(floor(position)) & 1) == 1;
    let delay = clamp(abs(across / (aspect * abs(s) + abs(c))) * 0.3, 0.0, 0.3);
    let local = clamp((eased - delay) / (1.0 - delay), 0.0, 1.0);
    let soft = mix(0.001, 0.3, fx.params.z * fx.params.z);
    let outgoing = fx_sample_from(uv);
    let incoming = fx_sample_to(uv);
    if odd {
        let edge = mix(1.0 + soft, -soft, local);
        return mix(outgoing, incoming, smoothstep(edge - soft, edge + soft, fract(position)));
    }
    let edge = mix(-soft, 1.0 + soft, local);
    return mix(incoming, outgoing, smoothstep(edge - soft, edge + soft, fract(position)));
}
