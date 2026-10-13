// @effect id: present
// @effect name: Present
// @effect kind: overlay
// @effect hidden: true
// Draws the view's content unchanged.

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    return fx_sample_from(uv);
}
