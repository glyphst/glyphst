// @effect id: glitch
// @effect name: Glitch
// @effect kind: transition
// @effect duration: 450
// @effect param: strength float 0 2 1 Strength
// Rows jitter sideways with split colours, each flipping to the incoming
// view at its own moment.

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
    let row = floor(uv.y * 36.0);
    let jitter = (fx_hash21(vec2<f32>(row, floor(t * 14.0))) - 0.5) * 2.0;
    let strength = sin(PI * t) * fx.params.x;
    let at = uv + vec2<f32>(jitter * 0.08 * strength, 0.0);
    let split = vec2<f32>(0.012 * strength, 0.0);
    let flipped = fx_hash21(vec2<f32>(row, 7.0)) < eased;
    var red: vec4<f32>;
    var green: vec4<f32>;
    var blue: vec4<f32>;
    if flipped {
        red = fx_sample_to(at + split);
        green = fx_sample_to(at);
        blue = fx_sample_to(at - split);
    } else {
        red = fx_sample_from(at + split);
        green = fx_sample_from(at);
        blue = fx_sample_from(at - split);
    }
    return vec4<f32>(red.r, green.g, blue.b, green.a);
}
