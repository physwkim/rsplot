// Fill shader: fill the band between a data-space polyline and its baseline.
//
// A non-instanced draw of 6 * (segment count) vertices builds two triangles per
// segment — a quad whose top edge is the curve (points[seg], points[seg+1]) and
// whose bottom edge is the baseline at the same x (baseline[seg], baseline[seg
// +1]). Unlike the line, there is no pixel-space expansion: the vertices are the
// data coordinates themselves, transformed straight to clip space. Drawn before
// the line so the stroke sits on top of its own fill (silx fill, §13 B1).

struct Params {
    ortho: mat4x4<f32>,
    color: vec4<f32>,        // linear premultiplied RGBA
    axis_log: vec2<f32>,     // 1.0 if that axis is log10, else 0.0
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> points: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read> baseline: array<f32>;

const INV_LN10: f32 = 0.4342944819032518;

fn apply_scale(p: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(
        select(p.x, log(p.x) * INV_LN10, params.axis_log.x > 0.5),
        select(p.y, log(p.y) * INV_LN10, params.axis_log.y > 0.5),
    );
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> @builtin(position) vec4<f32> {
    let seg = vid / 6u;
    let corner = vid % 6u;

    // Per-corner endpoint selector (0 = segment start, 1 = end) and whether the
    // corner is on the baseline (1) or the curve (0), for the two triangles
    // (curve0, base0, curve1) and (curve1, base0, base1).
    var endpoint = array<u32, 6>(0u, 0u, 1u, 1u, 0u, 1u);
    var is_base = array<u32, 6>(0u, 1u, 0u, 0u, 1u, 1u);

    let ep = endpoint[corner];
    let pt = points[seg + ep];
    // Clamp guards the 1-element placeholder bound when a curve is not filled.
    let bi = min(seg + ep, arrayLength(&baseline) - 1u);
    let yv = select(pt.y, baseline[bi], is_base[corner] == 1u);

    return params.ortho * vec4<f32>(apply_scale(vec2<f32>(pt.x, yv)), 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return params.color;
}
