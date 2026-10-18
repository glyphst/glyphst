// @effect id: ripple
// @effect name: Ripple
// @effect kind: transition
// @effect duration: 700
// @effect param: strength float 0 2 1 Ripple strength
// @effect param: glow colour #FFB000 Ring colour
// A wave front spreads from where the switch came from: the views ripple
// and split their colours along it, and a ring of light rides on it.

const PI: f32 = 3.14159265;

fn mixed(at: vec2<f32>, old: f32) -> vec4<f32> {
    return mix(fx_sample_to(at), fx_sample_from(at), old);
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
    let aspect = fx_aspect();
    let centre = fx_square(fx_origin());
    let p = fx_square(uv);
    let d = distance(p, centre);
    let front = eased * (fx_reach(centre) + 0.3) - 0.15;
    let old = smoothstep(front - 0.15, front + 0.15, d);
    let outward = normalize(p - centre + vec2<f32>(1e-5));
    let wave = sin(d * 44.0 - t * 18.0);
    let near = exp(-abs(d - front) * 7.0);
    let amount = 0.022 * fx.params.x * near * sin(PI * t);
    let offset = vec2<f32>(outward.x / aspect, outward.y) * wave * amount;
    let red = mixed(uv + offset * 1.6, old);
    let green = mixed(uv + offset, old);
    let blue = mixed(uv + offset * 0.4, old);
    var color = vec4<f32>(red.r, green.g, blue.b, green.a);
    let ring = exp(-abs(d - front) * 48.0) * sin(PI * t) * 0.55;
    color = vec4<f32>(color.rgb + fx_colour(fx.params.y) * ring * color.a, color.a);
    return color;
}
