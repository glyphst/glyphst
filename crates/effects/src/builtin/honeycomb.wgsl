// @effect id: honeycomb
// @effect name: Honeycomb
// @effect kind: transition
// @effect duration: 700
// @effect random: cell
// @effect param: cell float 0.015 0.1 0.04 Cell size
// @effect param: softness float 0 1 0.2 Edge softness
// Hexagonal cells turn over to the incoming view in a ring spreading from
// where the switch came from.

// The hexagon (axial coordinates) holding fractional axial point `h`.
fn hex_round(h: vec2<f32>) -> vec2<f32> {
    let cube = vec3<f32>(h.x, h.y, -h.x - h.y);
    var rounded = floor(cube + vec3<f32>(0.5));
    let error = abs(rounded - cube);
    if error.x > error.y && error.x > error.z {
        rounded.x = -rounded.y - rounded.z;
    } else if error.y > error.z {
        rounded.y = -rounded.x - rounded.z;
    }
    return rounded.xy;
}

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let t = fx.progress;
    if t <= 0.0 {
        return fx_sample_from(uv);
    }
    if t >= 1.0 {
        return fx_sample_to(uv);
    }
    let eased = fx_ease_in_out_cubic(t);
    let size = max(fx.params.x, 0.001);
    let p = fx_square(uv);
    let root3 = sqrt(3.0);
    let cell = hex_round(vec2<f32>(p.x * 2.0 / 3.0, -p.x / 3.0 + root3 / 3.0 * p.y) / size);
    let middle = vec2<f32>(cell.x * 1.5, root3 / 2.0 * cell.x + root3 * cell.y) * size;
    let centre = fx_square(fx_origin());
    let soft = mix(0.001, 0.5, fx.params.y * fx.params.y);
    let radius = eased * (fx_reach(centre) + size + 2.0 * soft) - soft;
    let old = smoothstep(radius - soft, radius + soft, distance(middle, centre));
    return mix(fx_sample_to(uv), fx_sample_from(uv), old);
}
