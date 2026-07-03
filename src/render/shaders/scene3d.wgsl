// 3D scene geometry shader (plot3d P0.2): lines and triangles, depth-tested,
// rendered into an offscreen color+depth target before being blitted into
// egui's (depth-less) pass.
//
// The MVP is `camera.matrix() × model`, already transposed to column-major and
// clip-corrected for wgpu's z∈[0,1] depth range (see `Mat4::to_gpu_clip_cols`);
// it arrives in the group(0) binding(0) uniform. Per-vertex position and color
// (linear, premultiplied) come from the vertex buffer.
//
// NOTE: `Params` is the shared group(0) uniform of the line/triangle pipelines
// AND the textured-image pipeline (`scene3d_image.wgsl`) — both bind the same
// buffer, so the two WGSL structs must stay layout-identical.

struct Params {
    mvp: mat4x4<f32>,
    // Linear fog datum, silx scene/function.py Fog (:79-151):
    // x = 0.9/(far-near) or 0, y = near (camera-space z), z = on/off, w unused.
    fog_info: vec4<f32>,
    // Fog colour = viewport background rgb (function.py:148-151); w unused.
    fog_color: vec4<f32>,
    // Row 2 of the view matrix: dot(view_row_z, vec4(pos, 1)) is the vertex's
    // camera-space z — silx's `vCameraPosition.z` fed to `fog()`.
    view_row_z: vec4<f32>,
};

@group(0) @binding(0)
var<uniform> params: Params;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Camera-space z for the fog term (linear in position, so interpolation is
    // exact — matches silx's per-fragment `vCameraPosition`).
    @location(1) cam_z: f32,
};

@vertex
fn vs_main(
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = params.mvp * vec4<f32>(pos, 1.0);
    out.color = color;
    out.cam_z = dot(params.view_row_z, vec4<f32>(pos, 1.0));
    return out;
}

// Linear fog, port of silx scene/function.py Fog._fragDecl (:79-93):
// factor = clamp(fogExtentInfo.x * (cameraPos.z - fogExtentInfo.y), 0, 1);
// rgb = mix(color.rgb, fogColor, factor), alpha untouched. Colours here are
// premultiplied, so the fog colour is scaled by alpha to stay premultiplied.
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
    return apply_fog(in.color, in.cam_z);
}
