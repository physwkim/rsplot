// Textured-quad shader — the wgpu analogue of silx `scene/primitives.py`
// `ImageData`/`ImageRgba`, which render a 2D image as one textured quad placed in
// the 3D scene (by default in the z=0 plane, pixel (col,row) → world (x,y)).
//
// The quad's world corners + UVs come in as vertex data (six vertices, two
// triangles); positions are projected by the usual clip MVP (group 0, shared
// with the line/triangle pipelines). The image is a premultiplied-linear RGBA8
// texture (group 1) so its sampled colour matches the geometry path's linear,
// premultiplied convention and round-trips through the blit. Nearest vs linear
// filtering is chosen by the sampler bound here (silx `InterpolationMixIn`).

struct Params {
    mvp: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec3<f32>, @location(1) uv: vec2<f32>) -> VsOut {
    var out: VsOut;
    out.clip = params.mvp * vec4<f32>(pos, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // The texture already holds premultiplied-linear RGBA; sample straight
    // through (premultiplied-alpha blend composites it correctly).
    return textureSample(tex, samp, in.uv);
}
