// Blit the offscreen 3D color target into egui's (color-only) render pass as a
// viewport-clipped full-screen triangle.
//
// egui-wgpu already calls set_viewport with the paint callback's rect, so NDC
// [-1,1] maps exactly to the widget rect; the offscreen texture is sized to the
// same pixel rect (see `Scene3dGpu::ensure_offscreen`), giving a 1:1 copy.

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    // Large full-screen triangle (3 vertices cover all of NDC [-1,1]^2).
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    let xy = corners[vid];
    var out: VsOut;
    out.clip = vec4<f32>(xy, 0.0, 1.0);
    // clip xy∈[-1,1] → uv∈[0,1], y flipped (texture origin is top-left).
    out.uv = vec2<f32>(xy.x * 0.5 + 0.5, 0.5 - 0.5 * xy.y);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(src_tex, src_sampler, in.uv);
}
