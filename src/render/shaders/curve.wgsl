// Polyline shader: expand each segment of a data-space polyline into a
// screen-space quad of a given pixel width, transformed to clip space via the
// shared data->NDC ortho matrix and filled with either a single uniform color
// or a per-vertex color interpolated along each segment.
//
// The points live in a read-only storage buffer; a non-instanced draw of
// 6 * (segment count) vertices builds two triangles per segment. Offsetting in
// pixel space (using the data-area viewport size) keeps the width uniform
// regardless of the data aspect ratio. Butt caps, no joins — for finely sampled
// curves the per-segment gap at a turn is sub-pixel; round joins/caps and
// anti-aliasing are later steps (doc/design.md §7·§13 B1).
//
// When `use_vertex_color` is set, each quad vertex takes the color of its own
// endpoint (point `seg` or `seg+1`), so the rasterizer interpolates a gradient
// along the segment (silx per-point line color, doc/design.md §13 B1).

struct Params {
    ortho: mat4x4<f32>,
    // Linear, premultiplied RGBA (already alpha-multiplied on the CPU side).
    color: vec4<f32>,
    axis_log: vec2<f32>,        // 1.0 if that axis is log10, else 0.0
    viewport_px: vec2<f32>,     // data-area size in physical pixels
    half_width_px: f32,         // half the line width, in physical pixels
    use_vertex_color: f32,      // >0.5 to take color from `vcolors` per vertex
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> points: array<vec2<f32>>;
// Per-vertex linear premultiplied RGBA, one per point. A 1-element placeholder
// when `use_vertex_color` is 0 (never sampled, but the binding must be present).
@group(0) @binding(2) var<storage, read> vcolors: array<vec4<f32>>;

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
    let pos_px = base + normal * (params.half_width_px * side[corner]);

    // This vertex's endpoint color. `select` evaluates both arms, so clamp the
    // index to the bound array length to stay in-bounds for the placeholder
    // buffer when per-vertex color is off.
    let idx = seg + ep;
    let ci = min(idx, arrayLength(&vcolors) - 1u);
    let color = select(params.color, vcolors[ci], params.use_vertex_color > 0.5);

    var out: VsOut;
    out.pos = vec4<f32>(pos_px / half_vp, 0.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
