// Colormapped 2D scalar image (slice 1, step 3).
//
// A quad covering the image's data-space rect is generated from the vertex
// index, transformed to NDC by the ortho matrix (data -> NDC, the single
// source of truth in core::transform). The fragment samples the scalar data
// texture (nearest or bilinear-on-data, mirroring silx interpolation),
// maps it to a [0, 1] LUT coordinate under the colormap normalization
// (linear / log10 / sqrt / gamma / arcsinh — mirrors silx GLPlotImage),
// and looks up the color in a 256x1 sRGB LUT texture (linear).

struct Params {
    ortho: mat4x4<f32>,
    rect: vec4<f32>,           // data-space extent: (x0, y0, x1, y1)
    axis_log: vec2<f32>,       // 1.0 if that axis is log10, else 0.0
    alpha: f32,
    cmap_min: f32,             // normalization transform of vmin
    cmap_one_over_range: f32,  // 1 / (norm(vmax) - norm(vmin)), or 0 if degenerate
    gamma: f32,                // exponent for norm == 3 (gamma)
    norm: u32,                 // normalization code: 0 linear, 1 log, 2 sqrt, 3 gamma, 4 arcsinh
    interp: u32,               // interpolation: 0 nearest, 1 linear (bilinear on data)
};

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

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var data_tex: texture_2d<f32>;   // R32Float, unfilterable
@group(0) @binding(2) var data_samp: sampler;          // unused: data is fetched
                                                       // via textureLoad below,
                                                       // kept for layout parity
@group(0) @binding(3) var lut_tex: texture_2d<f32>;    // 256x1 sRGB
@group(0) @binding(4) var lut_samp: sampler;           // filtering (linear)

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

    // t -> data space (rect.xy = lower-left, rect.zw = upper-right), then NDC.
    let dx = mix(params.rect.x, params.rect.z, t.x);
    let dy = mix(params.rect.y, params.rect.w, t.y);

    // Note: log axes warp only the quad corners here; texels interpolate
    // linearly across the quad, so an image under a log axis is corner-correct
    // but interior-distorted (doc/design.md §12·§13 A3 limitation).
    let eff = apply_scale(vec2<f32>(dx, dy));

    var out: VsOut;
    out.pos = params.ortho * vec4<f32>(eff, 0.0, 1.0);
    // uv.y = 0 at the bottom vertex, so texture row 0 (data[0]) is at the
    // bottom: y increases upward (matplotlib origin='lower' / silx convention).
    out.uv = t;
    return out;
}

// Map a raw scalar to its [0, 1] LUT coordinate under the colormap
// normalization. Mirrors silx GLPlotImage (and Colormap::normalize on the CPU,
// used for colorbar tick positions): the bounds are pre-transformed on the CPU
// into cmap_min / cmap_one_over_range, so per fragment only the value itself is
// transformed. log/sqrt guard their invalid domain by mapping to the low color.
fn normalize_value(raw: f32) -> f32 {
    if (params.norm == 1u) { // log10
        if (raw > 0.0) {
            return clamp(params.cmap_one_over_range * (log(raw) * INV_LN10 - params.cmap_min), 0.0, 1.0);
        }
        return 0.0;
    } else if (params.norm == 2u) { // sqrt
        if (raw >= 0.0) {
            return clamp(params.cmap_one_over_range * (sqrt(raw) - params.cmap_min), 0.0, 1.0);
        }
        return 0.0;
    } else if (params.norm == 3u) { // gamma
        return pow(clamp(params.cmap_one_over_range * (raw - params.cmap_min), 0.0, 1.0), params.gamma);
    } else if (params.norm == 4u) { // arcsinh
        // asinh is defined for all values, so there is no domain guard (silx
        // ArcsinhNormalization). cmap_min = asinh(vmin) is pre-transformed on
        // the CPU, matching log/sqrt.
        return clamp(params.cmap_one_over_range * (asinh(raw) - params.cmap_min), 0.0, 1.0);
    }
    // linear + fallback
    return clamp(params.cmap_one_over_range * (raw - params.cmap_min), 0.0, 1.0);
}

// Fetch one texel's scalar value by integer coordinate, clamped to the texture
// bounds so edge interpolation does not wrap. R32Float is not a filterable
// format, so the bilinear path is done by hand here via textureLoad rather than
// a linear sampler (avoids requiring the FLOAT32_FILTERABLE wgpu feature).
fn texel(coord: vec2<i32>, size: vec2<i32>) -> f32 {
    let c = clamp(coord, vec2<i32>(0, 0), size - vec2<i32>(1, 1));
    return textureLoad(data_tex, c, 0).r;
}

// Sample the scalar data at normalized uv. NEAREST takes the centre texel;
// LINEAR bilinearly interpolates the four neighbouring texels of the SCALAR
// data — silx interpolates the data and only then colormaps (GLPlotImage
// texture filtering is applied to the data texture before the cmap lookup).
fn sample_data(uv: vec2<f32>) -> f32 {
    let size = vec2<i32>(textureDimensions(data_tex));
    if (params.interp == 1u) { // linear
        // Texel centres sit at (i + 0.5) / size, so shift by -0.5 to put the
        // fractional weight between the surrounding texel centres.
        let p = uv * vec2<f32>(size) - vec2<f32>(0.5, 0.5);
        let base = vec2<i32>(floor(p));
        let f = p - floor(p);
        let v00 = texel(base, size);
        let v10 = texel(base + vec2<i32>(1, 0), size);
        let v01 = texel(base + vec2<i32>(0, 1), size);
        let v11 = texel(base + vec2<i32>(1, 1), size);
        let top = mix(v00, v10, f.x);
        let bot = mix(v01, v11, f.x);
        return mix(top, bot, f.y);
    }
    // nearest: the texel whose cell contains uv.
    let c = vec2<i32>(floor(uv * vec2<f32>(size)));
    return texel(c, size);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let v = sample_data(in.uv);
    let value = normalize_value(v);
    let rgb = textureSample(lut_tex, lut_samp, vec2<f32>(value, 0.5)).rgb;
    return vec4<f32>(rgb, params.alpha);
}
