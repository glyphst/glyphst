// @effect id: theme-tint
// @effect name: Theme colours
// @effect kind: overlay
// @effect themed: true
// @effect param: ink colour #1E2125 Ink
// @effect param: paper colour #F7F6F2 Paper
// @effect param: strength float 0 1 1 Strength
// Pages in the theme's colours: black ink becomes the theme's text and
// white paper its pane colour, so a dark theme reads them as a night mode
// in its own colours. Colourful parts keep half their colour.

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    let page = fx_sample_from(uv);
    let rgb = fx_unpremultiply(page);
    let lightness = fx_luminance(rgb);
    let duotone = mix(fx_colour(fx.params.x), fx_colour(fx.params.y), lightness);
    let themed = clamp(duotone + (rgb - vec3<f32>(lightness)) * 0.5, vec3<f32>(0.0), vec3<f32>(1.0));
    let tinted = mix(rgb, themed, fx.params.z);
    return vec4<f32>(tinted * page.a, page.a);
}
