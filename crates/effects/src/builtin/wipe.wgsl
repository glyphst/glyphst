// @effect id: wipe
// @effect name: Wipe
// @effect kind: transition
// @effect duration: 550
// @effect random: angle
// @effect param: angle float 0 360 0 Angle
// @effect param: softness float 0 1 0.3 Edge softness
// A soft edge sweeps across the view at an angle, revealing the incoming
// view behind it; going back sweeps the other way.

const PI: f32 = 3.14159265;

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let t = fx.progress;
    if t <= 0.0 {
        return fx_sample_from(uv);
    }
    if t >= 1.0 {
        return fx_sample_to(uv);
    }
    let eased = fx_ease_in_out_cubic(t);
    var angle = radians(fx.params.x);
    if fx.direction < 0.0 {
        angle += PI;
    }
    let along = vec2<f32>(cos(angle), sin(angle));
    let aspect = fx_aspect();
    let position = dot((uv - vec2<f32>(0.5)) * vec2<f32>(aspect, 1.0), along);
    // Half the view's extent along the sweep.
    let half = 0.5 * (aspect * abs(along.x) + abs(along.y));
    let soft = mix(0.001, 0.5, fx.params.y * fx.params.y);
    let edge = mix(-half - soft, half + soft, eased);
    let old = smoothstep(edge - soft, edge + soft, position);
    return mix(fx_sample_to(uv), fx_sample_from(uv), old);
}
