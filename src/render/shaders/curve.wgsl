// Polyline shader: expand each segment of a data-space polyline into a
// screen-space quad of a given pixel width, transformed to clip space via the
// shared data->NDC ortho matrix and filled with either a single uniform color
// or a per-vertex color interpolated along each segment.
//
// The points live in a read-only storage buffer; a non-instanced draw of
// 6 * (segment count) vertices builds two triangles per segment. Offsetting in
// pixel space (using the data-area viewport size) keeps the width uniform
// regardless of the data aspect ratio. Each segment is butt-capped here; round
// joins + round caps are added by `linecaps.wgsl` (an AA disc at every vertex,
// drawn alongside the line for solid styles). Each quad is expanded by 1 px beyond
// the nominal half-width; the fragment shader fades alpha smoothly to zero over
// that outermost pixel, giving analytical sub-pixel AA without MSAA.
//
// When `use_vertex_color` is set, each quad vertex takes the color of its own
// endpoint (point `seg` or `seg+1`), so the rasterizer interpolates a gradient
// along the segment (silx per-point line color, doc/design.md §13 B1).
//
// Dashing: each vertex carries its cumulative pixel arc length (CPU-computed in
// the current view), interpolated to the fragment. A `dash_cum` boundary set
// decides, by the phase within one period, whether a fragment is in an "on"
// (drawn) or "off" (gap) span; gaps are either discarded or filled with
// `gap_color` (silx linestyle / gapcolor, doc/design.md §13 B1).

struct Params {
    ortho: mat4x4<f32>,
    // Linear, premultiplied RGBA (already alpha-multiplied on the CPU side).
    color: vec4<f32>,
    gap_color: vec4<f32>,       // dashed-gap fill (premultiplied); used if use_gap_color
    dash_cum: vec4<f32>,        // cumulative dash boundaries; .w = period (0 = solid)
    axis_log: vec2<f32>,        // 1.0 if that axis is log10, else 0.0
    viewport_px: vec2<f32>,     // data-area size in physical pixels
    half_width_px: f32,         // half the line width, in physical pixels
    use_vertex_color: f32,      // >0.5 to take color from `vcolors` per vertex
    dash_offset: f32,           // phase offset added to arc length before the dash test
    use_gap_color: f32,         // >0.5 to fill gaps with gap_color, else discard
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> points: array<vec2<f32>>;
// Per-vertex linear premultiplied RGBA, one per point. A 1-element placeholder
// when `use_vertex_color` is 0 (never sampled, but the binding must be present).
@group(0) @binding(2) var<storage, read> vcolors: array<vec4<f32>>;
// Per-vertex cumulative pixel arc length. A 1-element placeholder when the line
// is solid (never sampled for the dash test, but the binding must be present).
@group(0) @binding(3) var<storage, read> arclen: array<f32>;

// 1 / ln(10), to turn the natural log into log10.
const INV_LN10: f32 = 0.4342944819032518;

// Map a data coordinate to the affine (transformed) space the ortho matrix
// expects: identity for a linear axis, log10 for a log axis. Must match
// core::transform::Axis::norm so chrome and shader agree (doc/design.md §4).
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

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    // Interpolated along the segment between the two endpoint colors.
    @location(0) color: vec4<f32>,
    // Cumulative pixel arc length, interpolated along the segment for dashing.
    @location(1) arc: f32,
    // Signed perpendicular distance from the segment centre, in pixels.
    // The quad is expanded by 1 px on each side beyond half_width_px to
    // accommodate the AA feather zone; |dist| == half_width_px + 1 at the
    // outer edge, 0 at the centre.
    @location(2) dist: f32,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    let seg = vid / 6u;
    let corner = vid % 6u;

    // Per-corner endpoint selector (0 = segment start, 1 = segment end) and the
    // perpendicular offset side, for the two triangles (start-, start+, end-)
    // and (end-, start+, end+). Function-local `var` arrays so the dynamic index
    // works on every backend.
    var endpoint = array<u32, 6>(0u, 0u, 1u, 1u, 0u, 1u);
    var side = array<f32, 6>(-1.0, 1.0, -1.0, -1.0, 1.0, 1.0);

    let half_vp = params.viewport_px * 0.5;
    // Endpoints in pixel space (NDC scaled by half the viewport).
    let px0 = to_ndc(points[seg]) * half_vp;
    let px1 = to_ndc(points[seg + 1u]) * half_vp;

    // Perpendicular unit vector in pixels; degenerate (zero-length) segments
    // collapse to a zero offset and draw nothing.
    let delta = px1 - px0;
    let len = length(delta);
    var normal = vec2<f32>(0.0, 0.0);
    if (len > 1e-6) {
        let dir = delta / len;
        normal = vec2<f32>(-dir.y, dir.x);
    }

    let ep = endpoint[corner];
    let base = select(px0, px1, ep == 1u);
    // Expand the quad by 1 px beyond half_width_px to give the AA feather zone
    // a full pixel of coverage without clipping.
    let expanded_half_w = params.half_width_px + 1.0;
    let side_f = side[corner];
    let pos_px = base + normal * (expanded_half_w * side_f);

    // This vertex's endpoint color and arc length. `select` evaluates both arms,
    // so clamp the index to the bound array length to stay in-bounds for the
    // placeholder buffers when per-vertex color / dashing is off.
    let idx = seg + ep;
    let ci = min(idx, arrayLength(&vcolors) - 1u);
    let color = select(params.color, vcolors[ci], params.use_vertex_color > 0.5);
    let ai = min(idx, arrayLength(&arclen) - 1u);

    var out: VsOut;
    out.pos = vec4<f32>(pos_px / half_vp, 0.0, 1.0);
    out.color = color;
    out.arc = arclen[ai];
    out.dist = expanded_half_w * side_f;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Analytical AA: fade linearly from fully opaque at the nominal edge
    // (|dist| == half_width_px) to fully transparent 1 px further out.
    // The quad was expanded by 1 px (see vs_main), so the feather zone is
    // entirely within the rasterised area.
    let aa = 1.0 - smoothstep(params.half_width_px, params.half_width_px + 1.0, abs(in.dist));

    let period = params.dash_cum.w;
    if (period <= 0.0) {
        return in.color * aa; // solid line
    }
    // Phase within one dash period (in physical pixels).
    let s = in.arc + params.dash_offset;
    let p = s - floor(s / period) * period;
    // "On" spans: [0, cum.x) and [cum.y, cum.z). Everything else is a gap.
    let on = (p < params.dash_cum.x) || (p >= params.dash_cum.y && p < params.dash_cum.z);
    if (on) {
        return in.color * aa;
    }
    if (params.use_gap_color > 0.5) {
        return params.gap_color * aa;
    }
    discard;
    // Unreachable payload: the fragment is discarded above, but a
    // function-terminal `discard` with no following `return` makes naga's
    // HLSL backend emit a path that FXC rejects (X3507 "not all control paths
    // return a value") on the Direct3D-12 backend. The trailing return keeps
    // the shader portable; its value is never written.
    return vec4<f32>(0.0);
}
