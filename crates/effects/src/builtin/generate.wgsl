// @effect id: generate
// @effect name: Generate
// @effect kind: transition
// @effect duration: 450
// @effect param: glow float 0 1 0.8 Scan line glow
// The glyph design's generation: the outgoing view dissolves into a dot
// field, and a scan line sweeps down drawing the incoming view behind it.

fn dots(pixel: vec2<f32>) -> f32 {
    let local = fract(pixel / 10.0) - vec2<f32>(0.5);
    return 1.0 - smoothstep(0.08, 0.16, length(local));
}

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let t = fx.progress;
    if t <= 0.0 {
        return fx_sample_from(uv);
    }
    if t >= 1.0 {
        return fx_sample_to(uv);
    }
    let scan = fx_smoothstep01(t) * 1.08 - 0.04;
    let incoming = fx_sample_to(uv);
    if uv.y < scan {
        return incoming;
    }
    // Below the line: the outgoing view fading into dots.
    let outgoing = fx_sample_from(uv);
    let fade = clamp(t * 2.0, 0.0, 1.0);
    let dot_color = vec4<f32>(0.227, 0.247, 0.275, 1.0) * dots(pixel);
    var below = mix(outgoing, dot_color, fade);
    // The scan line itself, amber.
    let distance = (uv.y - scan) * fx.resolution.y;
    // Strongest mid-way, gone at either end.
    let line = (1.0 - smoothstep(0.0, 3.0, distance)) * fx.params.x * sin(t * 3.14159);
    below = mix(below, vec4<f32>(1.0, 0.69, 0.0, 1.0), line);
    return below;
}
