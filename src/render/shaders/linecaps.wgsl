// Round line joins and caps: a filled, antialiased disc of the line's width
// centred on every polyline vertex.
//
// rsplot's polyline is drawn as independent butt-capped segment quads
// (`curve.wgsl`), so a sharp turn leaves a wedge-shaped gap on the outer side of
// the join and the two endpoints are flat. Stamping a disc of radius
// `half_width_px` at each vertex fills that wedge (a round join) and extends the
// two ends into half-discs (a round cap) — geometrically the union of all such
// discs with the segment quads is exactly a round-joined, round-capped stroke.
// This matches the default appearance of silx's pygfx `LineMaterial` (round
// joins + round caps; `doc/design.md` §7·§13 B1).
//
// A non-instanced draw of `6 × point-count` vertices builds one quad per vertex,
// read from the same storage buffer as the line. The disc reuses the line's
// analytical AA: the quad is expanded 1 px beyond the nominal half-width and the
// fragment fades alpha to zero over that outermost pixel, so the disc edge
// matches the segment edge feather exactly (no MSAA). The uniform layout is
// shared with `markers.wgsl` (`MarkerParams`); `half_size_px` carries the line
// half-width and `symbol` is unused.

struct Params {
    ortho: mat4x4<f32>,
    color: vec4<f32>,
    axis_log: vec2<f32>,      // 1.0 if that axis is log10, else 0.0
    viewport_px: vec2<f32>,   // data-area size in physical pixels
    half_size_px: f32,        // the line half-width, in physical pixels
    symbol: u32,              // unused (shared layout with markers.wgsl)
    use_vertex_color: f32,    // >0.5 to take each disc's color from `vcolors`
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> points: array<vec2<f32>>;
// Per-vertex linear premultiplied RGBA, one per point (silx per-point line
// color). A 1-element placeholder when `use_vertex_color` is 0. Mirrors
// curve.wgsl's vcolors so a join between two coloured vertices takes its own
// vertex's color.
@group(0) @binding(2) var<storage, read> vcolors: array<vec4<f32>>;

const INV_LN10: f32 = 0.4342944819032518;

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
    // Offset from the disc centre, in physical pixels; |xy| is the radial
    // distance the fragment shader feathers against.
    @location(0) local_px: vec2<f32>,
    // This vertex's color (constant across the quad).
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    let inst = vid / 6u;

    // Quad corners in local space (two triangles); function-local `var` so the
    // dynamic index works on every backend.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
    );
    let corner = corners[vid % 6u];

    let half_vp = params.viewport_px * 0.5;
    let center_px = to_ndc(points[inst]) * half_vp;
    // Expand the quad by 1 px beyond the nominal half-width for the AA feather
    // zone, exactly like the segment quads in curve.wgsl.
    let expanded_half_w = params.half_size_px + 1.0;
    let local_px = corner * expanded_half_w;
    let pos_px = center_px + local_px;

    // This vertex's per-point color; `vcolors` is a 1-element placeholder when
    // per-vertex color is off, so clamp the index to stay in-bounds.
    let ci = min(inst, arrayLength(&vcolors) - 1u);

    var out: VsOut;
    out.pos = vec4<f32>(pos_px / half_vp, 0.0, 1.0);
    out.local_px = local_px;
    out.color = vcolors[ci];
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Analytical AA: opaque inside the nominal radius, fading to zero 1 px
    // further out — identical to the segment feather in curve.wgsl.
    let d = length(in.local_px);
    let aa = 1.0 - smoothstep(params.half_size_px, params.half_size_px + 1.0, d);
    let color = select(params.color, in.color, params.use_vertex_color > 0.5);
    return color * aa;
}
