// @effect id: slide-over
// @effect name: Slide over
// @effect kind: transition
// @effect duration: 300
// @effect param: shadow float 0 1 0.35 Shadow strength
// @effect param: dim float 0 1 0.35 Dim outgoing view
// The incoming view slides over the outgoing one from the side, casting a
// soft shadow ahead of its content while the outgoing view dims.

// Width of the shadow, in view widths.
const SHADOW_WIDTH: f32 = 0.02;

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let t = fx.progress;
    if t <= 0.0 {
        return fx_sample_from(uv);
    }
    if t >= 1.0 {
        return fx_sample_to(uv);
    }
    let eased = fx_smoothstep01(t);
    let offset = fx.direction * (1.0 - eased);
    let to_uv = uv - vec2<f32>(offset, 0.0);
    let incoming = fx_sample_to(to_uv);
    var outgoing = fx_sample_from(uv);
    outgoing = vec4<f32>(outgoing.rgb * (1.0 - fx.params.y * eased), outgoing.a);
    // The incoming content's coverage a little further along its travel,
    // averaged over a few taps, is the shadow it casts.
    var cover = 0.0;
    for (var tap = 1; tap <= 4; tap++) {
        let along = f32(tap) * SHADOW_WIDTH / 4.0 * fx.direction;
        cover += fx_sample_to(to_uv + vec2<f32>(along, 0.0)).a;
    }
    let shadow = fx.params.x * cover / 4.0 * (1.0 - incoming.a);
    let shaded = vec4<f32>(outgoing.rgb * (1.0 - shadow), outgoing.a + shadow * (1.0 - outgoing.a));
    return incoming + shaded * (1.0 - incoming.a);
}
