// Direct RGBA image (silx addImage with an RGBA array): no colormap / LUT.
//
// Same quad as image.wgsl (the image's data-space rect mapped to NDC by the
// ortho matrix), but the fragment samples a sRGB RGBA texture directly and
// multiplies the global alpha. Used when the image is RGBA rather than a scalar
// field (doc/design.md §5·§6).

struct Params {
    ortho: mat4x4<f32>,
    rect: vec4<f32>,     // data-space extent: (x0, y0, x1, y1)
    axis_log: vec2<f32>, // 1.0 if that axis is log10, else 0.0
    alpha: f32,
};

// 1 / ln(10), to turn the natural log into log10.
const INV_LN10: f32 = 0.4342944819032518;

// Map a data coordinate to the affine (transformed) space the ortho matrix
// expects: identity for a linear axis, log10 for a log axis. Matches image.wgsl
// / core::transform::Axis::norm (doc/design.md §4).
fn apply_scale(p: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(
        select(p.x, log(p.x) * INV_LN10, params.axis_log.x > 0.5),
        select(p.y, log(p.y) * INV_LN10, params.axis_log.y > 0.5),
    );
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var rgba_tex: texture_2d<f32>; // Rgba8UnormSrgb
@group(0) @binding(2) var rgba_samp: sampler;         // non-filtering (nearest)

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    // Two triangles forming the unit quad in [0,1]^2.
    var verts = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    let t = verts[vid];

    let dx = mix(params.rect.x, params.rect.z, t.x);
    let dy = mix(params.rect.y, params.rect.w, t.y);
    let eff = apply_scale(vec2<f32>(dx, dy));

    var out: VsOut;
    out.pos = params.ortho * vec4<f32>(eff, 0.0, 1.0);
    // uv.y = 0 at the bottom vertex, so texture row 0 is at the bottom (origin
    // lower-left), matching the scalar image convention.
    out.uv = t;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(rgba_tex, rgba_samp, in.uv);
    return vec4<f32>(c.rgb, c.a * params.alpha);
}
