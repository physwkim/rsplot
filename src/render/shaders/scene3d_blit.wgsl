// Blit the offscreen 3D color target into egui's (color-only) render pass as a
// viewport-clipped full-screen triangle.
//
// egui-wgpu already calls set_viewport with the paint callback's rect, so NDC
// [-1,1] maps exactly to the widget rect; the offscreen texture is sized to the
// same pixel rect (see `Scene3dGpu::ensure_offscreen`), giving a 1:1 copy.
//
// The offscreen target is the sRGB variant of egui's surface format
// (`Scene3dPipeline::offscreen_format`), so the sample is linear colour. Which
// fragment entry point the blit pipeline uses depends on the surface
// (`blit_entry_point`): an sRGB surface encodes on write, so the linear sample
// passes through; egui-wgpu's default non-sRGB surface holds gamma-encoded
// values (egui's own shader writes gamma there), so the blit encodes the sample
// to sRGB itself — the same split as egui-wgpu's `fs_main_linear_framebuffer` /
// `fs_main_gamma_framebuffer`. Alpha is left as is in both.

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

// Linear → sRGB transfer function (IEC 61966-2-1), per channel.
fn linear_to_srgb(linear: vec3<f32>) -> vec3<f32> {
    let lower = linear * 12.92;
    let higher = 1.055 * pow(linear, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(higher, lower, linear <= vec3<f32>(0.0031308));
}

// sRGB surface: the hardware encodes on write, so pass the linear sample through.
@fragment
fn fs_main_linear_target(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(src_tex, src_sampler, in.uv);
}

// Non-sRGB surface (egui-wgpu's default): encode the linear sample to sRGB here.
@fragment
fn fs_main_gamma_target(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src_tex, src_sampler, in.uv);
    return vec4<f32>(linear_to_srgb(c.rgb), c.a);
}
