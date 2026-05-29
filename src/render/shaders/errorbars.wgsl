// Error-bar shader: draw per-point error bars (a bar plus two end caps) as
// fixed-pixel-width line segments.
//
// Each segment is two consecutive `segs` entries; an entry `vec4(anchor_x,
// anchor_y, offset_px_x, offset_px_y)` is a data anchor plus a pixel-space
// offset. The anchor is mapped through the shared data->NDC ortho matrix to
// pixel space, then the pixel offset is added, so the caps keep a constant
// pixel size at any zoom. A non-instanced draw of 6 * (segment count) vertices
// builds two triangles per segment, exactly like the line (doc/design.md §13 B1).

struct Params {
    ortho: mat4x4<f32>,
    color: vec4<f32>,        // linear premultiplied RGBA
    axis_log: vec2<f32>,     // 1.0 if that axis is log10, else 0.0
    viewport_px: vec2<f32>,  // data-area size in physical pixels
    half_width_px: f32,      // half the error-bar line width, in physical pixels
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> segs: array<vec4<f32>>;

// 1 / ln(10), to turn the natural log into log10.
const INV_LN10: f32 = 0.4342944819032518;

// Map a data coordinate to the affine (transformed) space the ortho matrix
// expects: identity for a linear axis, log10 for a log axis. Matches `apply_scale`
// in curve.wgsl / core::transform::Axis::norm (doc/design.md §4).
fn apply_scale(p: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(
        select(p.x, log(p.x) * INV_LN10, params.axis_log.x > 0.5),
        select(p.y, log(p.y) * INV_LN10, params.axis_log.y > 0.5),
    );
}

fn to_ndc(p: vec2<f32>) -> vec2<f32> {
    let clip = params.ortho * vec4<f32>(apply_scale(p), 0.0, 1.0);
    return clip.xy / clip.w;
}

// Pixel-space position of one segment endpoint: the data anchor mapped to pixels
// plus the endpoint's pixel offset (so caps keep a constant pixel size).
fn endpoint_px(e: vec4<f32>, half_vp: vec2<f32>) -> vec2<f32> {
    return to_ndc(e.xy) * half_vp + e.zw;
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> @builtin(position) vec4<f32> {
    let seg = vid / 6u;
    let corner = vid % 6u;

    // Per-corner endpoint selector (0 = segment start, 1 = end) and the
    // perpendicular offset side, for the two triangles (start-, start+, end-)
    // and (end-, start+, end+). Function-local `var` arrays so the dynamic index
    // works on every backend.
    var endpoint = array<u32, 6>(0u, 0u, 1u, 1u, 0u, 1u);
    var side = array<f32, 6>(-1.0, 1.0, -1.0, -1.0, 1.0, 1.0);

    let half_vp = params.viewport_px * 0.5;
    // Two endpoint entries per segment; clamp guards the 1-element placeholder
    // buffer bound when the curve has no error bars (the draw is skipped then,
    // but the binding must still be in range).
    let n = arrayLength(&segs);
    let i0 = min(2u * seg, n - 1u);
    let i1 = min(2u * seg + 1u, n - 1u);
    let px0 = endpoint_px(segs[i0], half_vp);
    let px1 = endpoint_px(segs[i1], half_vp);

    // Perpendicular unit vector in pixels; degenerate (zero-length) segments
    // collapse to a zero offset and draw nothing (e.g. a zero-error point).
    let delta = px1 - px0;
    let len = length(delta);
    var normal = vec2<f32>(0.0, 0.0);
    if (len > 1e-6) {
        let dir = delta / len;
        normal = vec2<f32>(-dir.y, dir.x);
    }

    let ep = endpoint[corner];
    let base = select(px0, px1, ep == 1u);
    let pos_px = base + normal * (params.half_width_px * side[corner]);

    return vec4<f32>(pos_px / half_vp, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return params.color;
}
