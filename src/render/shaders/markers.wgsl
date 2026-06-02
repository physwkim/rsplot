// Marker shader: draw a pixel-sized symbol at each polyline point.
//
// A non-instanced draw of 6 * point-count vertices builds one screen-space quad
// per point (read from the same storage buffer as the line). The quad spans
// local coords uv ∈ [-1, 1]²; the fragment shader fills the symbol's signed
// region and discards the rest. Sizes are in physical pixels, uniform under any
// data aspect ratio or zoom (doc/design.md §13 B2).

struct Params {
    ortho: mat4x4<f32>,
    color: vec4<f32>,
    axis_log: vec2<f32>,      // 1.0 if that axis is log10, else 0.0
    viewport_px: vec2<f32>,   // data-area size in physical pixels
    half_size_px: f32,        // half the marker size, in physical pixels
    // 0 circle, 1 square, 2 cross, 3 plus, 4 triangle, 5 diamond, 6 point,
    // 7 pixel, 8 vertical line, 9 horizontal line, 10..13 tick left/right/up/down,
    // 14..17 caret left/right/up/down (matches Symbol::code).
    symbol: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> points: array<vec2<f32>>;

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
    @location(0) uv: vec2<f32>,
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
    let pos_px = center_px + corner * params.half_size_px;

    var out: VsOut;
    out.pos = vec4<f32>(pos_px / half_vp, 0.0, 1.0);
    out.uv = corner;
    return out;
}

// Signed cross-product edge test, for the triangle symbol.
fn edge(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    return (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
}

fn inside(uv: vec2<f32>) -> bool {
    // Bar half-thickness for cross/plus symbols.
    let th = 0.32;
    // Pixel-space offset from the marker center: silx's symbol shaders test
    // against fixed-pixel thresholds (`size * (coord - 0.5)` in GLPlotCurve.py),
    // which equals `half_size_px * uv` here since `marker_size = 2 * half_size_px`
    // and `coord - 0.5 = uv / 2`. Used by the stroke/caret symbols (8..17).
    let pix = uv * params.half_size_px;
    switch params.symbol {
        case 0u: { // circle
            return dot(uv, uv) <= 1.0;
        }
        case 1u: { // square (fills the whole quad)
            return true;
        }
        case 2u: { // cross (X): near either diagonal
            let d1 = abs(uv.x - uv.y) * 0.70710677;
            let d2 = abs(uv.x + uv.y) * 0.70710677;
            return min(d1, d2) <= th;
        }
        case 3u: { // plus (+): horizontal or vertical bar
            return abs(uv.x) <= th || abs(uv.y) <= th;
        }
        case 4u: { // upward triangle
            let a = vec2<f32>(0.0, 1.0);
            let b = vec2<f32>(-0.866, -0.5);
            let c = vec2<f32>(0.866, -0.5);
            let e1 = edge(uv, a, b);
            let e2 = edge(uv, b, c);
            let e3 = edge(uv, c, a);
            return (e1 >= 0.0 && e2 >= 0.0 && e3 >= 0.0)
                || (e1 <= 0.0 && e2 <= 0.0 && e3 <= 0.0);
        }
        case 5u: { // diamond (rotated square): silx |cx| + |cy| < 0.5
            return abs(uv.x) + abs(uv.y) <= 1.0;
        }
        case 6u: { // point: a small filled circle (size shrunk on the CPU side)
            return dot(uv, uv) <= 1.0;
        }
        case 7u: { // pixel: a single-pixel square (size set to 1px on the CPU side)
            return true;
        }
        case 8u: { // vertical line: thin vertical stroke (silx |pix.x| <= 1)
            return abs(pix.x) <= 1.0;
        }
        case 9u: { // horizontal line: thin horizontal stroke (silx |pix.y| <= 1)
            return abs(pix.y) <= 1.0;
        }
        case 10u: { // tick left: horizontal stroke on the left half (silx pix.x <= 0.5)
            return pix.x <= 0.5 && abs(pix.y) <= 1.0;
        }
        case 11u: { // tick right: horizontal stroke on the right half (silx pix.x >= -0.5)
            return pix.x >= -0.5 && abs(pix.y) <= 1.0;
        }
        case 12u: { // tick up: vertical stroke on the upper half (silx pix.y <= 0.5)
            return pix.y <= 0.5 && abs(pix.x) <= 1.0;
        }
        case 13u: { // tick down: vertical stroke on the lower half (silx pix.y >= -0.5)
            return pix.y >= -0.5 && abs(pix.x) <= 1.0;
        }
        case 14u: { // caret left: open wedge, silx |pix.x| - |pix.y| >= -0.1, pix.x > 0.5
            return pix.x > 0.5 && (abs(pix.x) - abs(pix.y)) >= -0.1;
        }
        case 15u: { // caret right: silx |pix.x| - |pix.y| >= -0.1, pix.x < 0.5
            return pix.x < 0.5 && (abs(pix.x) - abs(pix.y)) >= -0.1;
        }
        case 16u: { // caret up: silx |pix.y| - |pix.x| >= -0.1, pix.y > 0.5
            return pix.y > 0.5 && (abs(pix.y) - abs(pix.x)) >= -0.1;
        }
        case 17u: { // caret down: silx |pix.y| - |pix.x| >= -0.1, pix.y < 0.5
            return pix.y < 0.5 && (abs(pix.y) - abs(pix.x)) >= -0.1;
        }
        default: {
            return dot(uv, uv) <= 1.0;
        }
    }
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (!inside(in.uv)) {
        discard;
    }
    return params.color;
}
