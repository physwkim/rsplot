// Minimal shader that fills the data area with a solid color (slice 1, step 1).
//
// egui-wgpu already calls set_viewport with the paint callback's rect, so a
// full-screen triangle covering all of NDC paints only the viewport (= the data
// rect). The color comes from the group(0) binding(0) params block (premultiplied
// RGBA in the target's encoding), bound as a read-only storage buffer like every
// rsplot params block (see `CurveParams` in gpu_curve.rs).

struct Params {
    color: vec4<f32>,
};

@group(0) @binding(0)
var<storage, read> params: Params;

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> @builtin(position) vec4<f32> {
    // Large full-screen triangle (3 vertices cover all of NDC [-1,1]^2).
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    return vec4<f32>(corners[vid], 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return params.color;
}
