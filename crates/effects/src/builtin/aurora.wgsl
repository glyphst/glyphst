// @effect id: aurora
// @effect name: Aurora
// @effect kind: background
// @effect animated: true
// @effect param: speed float 0 3 0.4 Speed
// @effect param: brightness float 0 1 0.35 Brightness
// Slowly drifting colour bands, as a sample animated wallpaper.

fn band(uv: vec2<f32>, phase: f32, height: f32) -> f32 {
    let wave = sin(uv.x * 3.1 + phase) * 0.08 + sin(uv.x * 7.3 - phase * 1.7) * 0.03;
    return exp(-pow((uv.y - height - wave) * 9.0, 2.0));
}

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let t = fx.time * fx.params.x;
    let base = mix(vec3<f32>(0.04, 0.05, 0.10), vec3<f32>(0.08, 0.06, 0.14), uv.y);
    let green = band(uv, t, 0.35) * vec3<f32>(0.15, 0.85, 0.55);
    let violet = band(uv, t * 0.7 + 2.0, 0.55) * vec3<f32>(0.55, 0.35, 0.95);
    let blue = band(uv, t * 1.3 + 4.0, 0.72) * vec3<f32>(0.2, 0.5, 1.0);
    let color = base + (green + violet + blue) * fx.params.y;
    return vec4<f32>(color, 1.0);
}
