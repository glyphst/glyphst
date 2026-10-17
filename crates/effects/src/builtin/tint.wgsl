// @effect id: tint
// @effect name: Custom tint
// @effect kind: overlay
// @effect param: colour colour #F1E3C6 Colour
// @effect param: strength float 0 1 0.8 Strength
// Pages tinted by a colour of your own: white paper takes the colour and
// dark ink stays dark.

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let page = fx_sample_from(uv);
    let rgb = fx_unpremultiply(page);
    let tinted = mix(rgb, rgb * fx_colour(fx.params.x), fx.params.y);
    return vec4<f32>(tinted * page.a, page.a);
}
