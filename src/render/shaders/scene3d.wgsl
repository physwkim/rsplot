// 3D scene geometry shader (plot3d P0.2): lines and triangles, depth-tested,
// rendered into an offscreen color+depth target before being blitted into
// egui's (depth-less) pass.
//
// The MVP is `camera.matrix() × model`, already transposed to column-major and
// clip-corrected for wgpu's z∈[0,1] depth range (see `Mat4::to_gpu_clip_cols`);
// it arrives in the group(0) binding(0) uniform. Per-vertex position and color
// (linear, premultiplied) come from the vertex buffer.

struct Params {
    mvp: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> params: Params;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = params.mvp * vec4<f32>(pos, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
