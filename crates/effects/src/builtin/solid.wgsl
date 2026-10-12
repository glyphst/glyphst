// @effect id: solid
// @effect name: Solid colour
// @effect kind: background
// @effect param: red float 0 1 0.3 Red
// @effect param: green float 0 1 0.3 Green
// @effect param: blue float 0 1 0.32 Blue

fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(fx.params.xyz, 1.0);
}
