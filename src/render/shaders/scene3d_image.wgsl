// Textured-quad shader — the wgpu analogue of silx `scene/primitives.py`
// `ImageData`/`ImageRgba`, which render a 2D image as one textured quad placed in
// the 3D scene (by default in the z=0 plane, pixel (col,row) → world (x,y)).
//
// The quad's world corners + UVs come in as vertex data (six vertices, two
// triangles); positions are projected by the usual clip MVP (group 0, shared
// with the line/triangle pipelines — `Params` must stay layout-identical to
// `scene3d.wgsl`). The image is a premultiplied-linear RGBA8 texture (group 1)
// so its sampled colour matches the geometry path's linear, premultiplied
// convention and round-trips through the blit. Nearest vs linear filtering is
// chosen by the sampler bound here (silx `InterpolationMixIn`).
//
// silx applies the viewport fog to `_Image` like every other primitive
// (primitives.py `_Image._shaders` composes `sceneDecl`/`scenePostCall`), so the
// same linear-fog term runs here.

struct Params {
    mvp: mat4x4<f32>,
    // Linear fog datum (see scene3d.wgsl — shared buffer, identical layout).
    fog_info: vec4<f32>,
    fog_color: vec4<f32>,
    view_row_z: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) cam_z: f32,
};

@vertex
fn vs_main(@location(0) pos: vec3<f32>, @location(1) uv: vec2<f32>) -> VsOut {
    var out: VsOut;
    out.clip = params.mvp * vec4<f32>(pos, 1.0);
    out.uv = uv;
    out.cam_z = dot(params.view_row_z, vec4<f32>(pos, 1.0));
    return out;
}

// Linear fog, port of silx scene/function.py Fog._fragDecl (:79-93); colours
// are premultiplied, so the fog colour is scaled by alpha (see scene3d.wgsl).
fn apply_fog(color: vec4<f32>, cam_z: f32) -> vec4<f32> {
    if (params.fog_info.z == 0.0) {
        return color;
    }
    let factor = clamp(params.fog_info.x * (cam_z - params.fog_info.y), 0.0, 1.0);
    let rgb = mix(color.rgb, params.fog_color.rgb * color.a, factor);
    return vec4<f32>(rgb, color.a);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // The texture already holds premultiplied-linear RGBA; sample straight
    // through (premultiplied-alpha blend composites it correctly).
    return apply_fog(textureSample(tex, samp, in.uv), in.cam_z);
}
