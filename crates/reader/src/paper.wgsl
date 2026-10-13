// Paper: page rectangles under the page vectors, in clip space, with the
// colour of the current theme's page.

struct Paper {
    color: vec4<f32>,
}

@group(0) @binding(0) var<uniform> paper: Paper;

@vertex
fn vs_paper(@location(0) position: vec2<f32>) -> @builtin(position) vec4<f32> {
    return vec4<f32>(position, 0.0, 1.0);
}

@fragment
fn fs_paper() -> @location(0) vec4<f32> {
    return paper.color;
}
