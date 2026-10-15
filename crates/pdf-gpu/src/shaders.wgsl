// Page-space vector rendering. Geometry never changes with zoom; the camera
// maps page points to device pixels: pixel = (axes * page - origin) * scale,
// where axes turns the page by quarter turns.

struct Camera {
    origin: vec2<f32>,
    scale: f32,
    _padding: f32,
    viewport: vec2<f32>,
    _padding2: vec2<f32>,
    // Row-major 2x2 rotation.
    axes: vec4<f32>,
}

@group(0) @binding(0) var<uniform> camera: Camera;

fn to_pixels(page: vec2<f32>) -> vec2<f32> {
    let view = vec2<f32>(
        camera.axes.x * page.x + camera.axes.y * page.y,
        camera.axes.z * page.x + camera.axes.w * page.y,
    );
    return (view - camera.origin) * camera.scale;
}

fn to_clip(pixels: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(
        pixels.x / camera.viewport.x * 2.0 - 1.0,
        1.0 - pixels.y / camera.viewport.y * 2.0,
        0.0,
        1.0,
    );
}

fn premultiply(color: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(color.rgb * color.a, color.a);
}

// Fills: triangles tessellated in page space, antialiased by MSAA.

struct FillOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_fill(@location(0) position: vec2<f32>, @location(1) color: vec4<f32>) -> FillOut {
    var out: FillOut;
    out.position = to_clip(to_pixels(position));
    out.color = premultiply(color);
    return out;
}

@fragment
fn fs_fill(in: FillOut) -> @location(0) vec4<f32> {
    return in.color;
}

// Coverage fills: one quad per path whose pixels average the path's winding
// over their footprint, integrated from its edges (HEPR's analytic fills),
// so their edges are as smooth as MuPDF's without relying on MSAA.

struct CoveragePath {
    bounds: vec4<f32>,
    segments: vec2<u32>,
    bands: vec2<u32>,
    color: u32,
    even_odd: u32,
    area: f32,
    _padding: u32,
}

struct Edge {
    p0: vec2<f32>,
    p1: vec2<f32>,
}

@group(1) @binding(0) var<storage, read> paths: array<CoveragePath>;
@group(1) @binding(1) var<storage, read> edges: array<Edge>;
@group(1) @binding(2) var<storage, read> bands: array<vec2<u32>>;
@group(1) @binding(3) var<storage, read> band_entries: array<u32>;

// The integral of clamp(u, 0, 1).
fn ramp(u: f32) -> f32 {
    let c = clamp(u, 0.0, 1.0);
    return 0.5 * c * c + max(u - 1.0, 0.0);
}

// An edge's part of a box's winding over rows [low, high]: the integral over
// those rows of the share of the box's width (from `left` to `right`,
// `inv_width` its reciprocal) lying left of the edge, signed by the edge's
// direction. Most edges a pixel sums lie wholly left or right of it.
fn edge_coverage(edge: Edge, left: f32, right: f32, inv_width: f32, low: f32, high: f32) -> f32 {
    let a = edge.p0;
    let b = edge.p1;
    let y0 = clamp(a.y, low, high);
    let y1 = clamp(b.y, low, high);
    let dy = y1 - y0;
    if dy == 0.0 || max(a.x, b.x) <= left {
        return 0.0;
    }
    if min(a.x, b.x) >= right {
        return dy;
    }
    let slope = (b.x - a.x) / (b.y - a.y);
    let u0 = (a.x + (y0 - a.y) * slope - left) * inv_width;
    let u1 = (a.x + (y1 - a.y) * slope - left) * inv_width;
    let du = u1 - u0;
    var mean = clamp(0.5 * (u0 + u1), 0.0, 1.0);
    if abs(du) > 1e-4 {
        mean = (ramp(u1) - ramp(u0)) / du;
    }
    return dy * mean;
}

// The coverage of fill `index` over the pixel at page point `page` whose
// footprint is `footprint` page units, times its color.
fn fill_color(index: u32, page: vec2<f32>, footprint: vec2<f32>) -> vec4<f32> {
    let path = paths[index];
    let low = page - 0.5 * footprint;
    let high = page + 0.5 * footprint;
    let inv = 1.0 / footprint;
    let hull = path.bounds.zw - path.bounds.xy;
    var winding = 0.0;
    if all(hull <= footprint) {
        // A path no larger than the pixel: its area, by its overlap.
        let overlap = max(min(high, path.bounds.zw) - max(low, path.bounds.xy), vec2<f32>(0.0));
        let share = overlap / max(hull, vec2<f32>(1e-12));
        winding = path.area * inv.x * inv.y * share.x * share.y;
    } else if path.bands.y == 0u {
        for (var i = 0u; i < path.segments.y; i++) {
            winding += edge_coverage(
                edges[path.segments.x + i],
                low.x,
                high.x,
                inv.x,
                low.y,
                high.y,
            );
        }
        winding *= inv.y;
    } else {
        // Each band sums its edges over its own rows only.
        let count = path.bands.y;
        let height = hull.y / f32(count);
        let last = f32(count - 1u);
        let first_band = u32(clamp(floor((low.y - path.bounds.y) / height), 0.0, last));
        let last_band = u32(clamp(floor((high.y - path.bounds.y) / height), 0.0, last));
        for (var b = first_band; b <= last_band; b++) {
            let band_low = path.bounds.y + f32(b) * height;
            var rows_low = max(low.y, band_low);
            var rows_high = min(high.y, band_low + height);
            if b == 0u {
                rows_low = low.y;
            }
            if b == count - 1u {
                rows_high = high.y;
            }
            let band = bands[path.bands.x + b];
            for (var i = 0u; i < band.y; i++) {
                let edge = edges[band_entries[band.x + i]];
                winding += edge_coverage(edge, low.x, high.x, inv.x, rows_low, rows_high);
            }
        }
        winding *= inv.y;
    }
    var coverage = min(abs(winding), 1.0);
    if path.even_odd == 1u {
        let m = abs(winding);
        coverage = abs(m - 2.0 * floor(0.5 * m + 0.5));
    }
    return premultiply(unpack4x8unorm(path.color)) * coverage;
}

// Vectors: one instanced quad per record, in paint order. A stroke segment
// gets analytic capsule coverage (round joins come from overlapping
// capsules; butt ends are cut flat); a coverage fill gets its path's quad.

const BUTT_START: u32 = 1u;
const BUTT_END: u32 = 2u;
const FILL: u32 = 4u;
const AA_MARGIN: f32 = 0.5;

struct VectorOut {
    @builtin(position) position: vec4<f32>,
    // Strokes: distance along and signed distance across the segment, in
    // pixels. Fills: the page point.
    @location(0) local: vec2<f32>,
    // Strokes: the segment's length and half width in pixels. Fills: a
    // pixel's size in page units (the camera only scales and turns by
    // quarter turns, so pixels are page-aligned squares).
    @location(1) @interpolate(flat) length_half_width: vec2<f32>,
    @location(2) @interpolate(flat) flags: u32,
    @location(3) color: vec4<f32>,
    @location(4) @interpolate(flat) path: u32,
}

fn fill_vertex(vertex: u32, path: u32) -> VectorOut {
    let bounds = paths[path].bounds;
    // Every pixel whose box meets the path has its center within half a
    // pixel of it; a little more for rounding.
    let margin = 0.75 / camera.scale;
    var corner = bounds.xy - vec2<f32>(margin);
    if (vertex & 1u) == 1u {
        corner.x = bounds.z + margin;
    }
    if (vertex & 2u) == 2u {
        corner.y = bounds.w + margin;
    }
    var out: VectorOut;
    out.position = to_clip(to_pixels(corner));
    out.local = corner;
    out.length_half_width = vec2<f32>(1.0 / camera.scale);
    out.flags = FILL;
    out.path = path;
    return out;
}

@vertex
fn vs_vector(
    @builtin(vertex_index) vertex: u32,
    @location(0) p0: vec2<f32>,
    @location(1) p1: vec2<f32>,
    @location(2) half_width: f32,
    @location(3) color: vec4<f32>,
    @location(4) flags: u32,
    @location(5) path: u32,
) -> VectorOut {
    if (flags & FILL) != 0u {
        return fill_vertex(vertex, path);
    }
    let s0 = to_pixels(p0);
    let s1 = to_pixels(p1);
    let delta = s1 - s0;
    let segment_length = length(delta);
    var direction = vec2<f32>(1.0, 0.0);
    if segment_length > 1e-6 {
        direction = delta / segment_length;
    }
    let normal = vec2<f32>(-direction.y, direction.x);
    // MuPDF's rule: lines under 0.1 px (including zero-width hairlines) draw
    // one pixel wide; other sub-pixel lines draw one pixel wide with their
    // coverage, as antialiasing at their real width would.
    let true_width = 2.0 * half_width * camera.scale;
    var half_width_px = 0.5 * true_width;
    var thin = 1.0;
    if true_width < 0.1 {
        half_width_px = 0.5;
    } else if true_width < 1.0 {
        half_width_px = 0.5;
        thin = true_width;
    }
    let extent = half_width_px + AA_MARGIN;

    var along = -extent;
    if (vertex & 1u) == 1u {
        along = segment_length + extent;
    }
    var across = -extent;
    if (vertex & 2u) == 2u {
        across = extent;
    }

    var out: VectorOut;
    out.position = to_clip(s0 + direction * along + normal * across);
    out.local = vec2<f32>(along, across);
    out.length_half_width = vec2<f32>(segment_length, half_width_px);
    out.flags = flags;
    out.color = premultiply(color) * thin;
    return out;
}

fn stroke_color(in: VectorOut) -> vec4<f32> {
    let t = in.local.x;
    let d = in.local.y;
    let segment_length = in.length_half_width.x;
    let half_width = in.length_half_width.y;
    var distance = abs(d);
    var cap = 1.0;
    if t < 0.0 {
        if (in.flags & BUTT_START) != 0u {
            cap = clamp(t + 0.5, 0.0, 1.0);
        } else {
            distance = length(vec2<f32>(t, d));
        }
    } else if t > segment_length {
        if (in.flags & BUTT_END) != 0u {
            cap = clamp(segment_length - t + 0.5, 0.0, 1.0);
        } else {
            distance = length(vec2<f32>(t - segment_length, d));
        }
    }
    return in.color * (clamp(half_width + 0.5 - distance, 0.0, 1.0) * cap);
}

@fragment
fn fs_vector(in: VectorOut) -> @location(0) vec4<f32> {
    var color: vec4<f32>;
    if (in.flags & FILL) != 0u {
        color = fill_color(in.path, in.local, in.length_half_width);
    } else {
        color = stroke_color(in);
    }
    // No discard: a shader that may discard runs before the stencil test
    // on many GPUs; a transparent premultiplied color blends to nothing.
    return color;
}

// Raster layers: premultiplied MuPDF pixmaps on a page-space quad.

@group(1) @binding(0) var layer_texture: texture_2d<f32>;
@group(1) @binding(1) var layer_sampler: sampler;

struct LayerOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_layer(@location(0) position: vec2<f32>, @location(1) uv: vec2<f32>) -> LayerOut {
    var out: LayerOut;
    out.position = to_clip(to_pixels(position));
    out.uv = uv;
    return out;
}

@fragment
fn fs_layer(in: LayerOut) -> @location(0) vec4<f32> {
    return textureSample(layer_texture, layer_sampler, in.uv);
}
