// Volume ray-caster — front-to-back alpha compositing over a 3D RGBA texture.
//
// A full-screen triangle generates one camera ray per pixel from the inverse
// view-projection matrix. Each ray is clipped to the volume's axis-aligned box
// and marched in fixed steps, sampling straight-alpha RGBA and accumulating
// PREMULTIPLIED colour so the result blends with `PREMULTIPLIED_ALPHA_BLENDING`
// straight into egui's render pass (no offscreen target, no depth).

struct Uniforms {
    // Inverse of the camera clip matrix P·V (OpenGL clip, z in [-1, 1]).
    inv_mvp: mat4x4<f32>,
    cam_pos: vec4<f32>, // world-space camera position (xyz; w unused)
    vol_min: vec4<f32>, // world-space AABB min (xyz)
    vol_max: vec4<f32>, // world-space AABB max (xyz)
    // x = step count, y = alpha scale, z = sample-alpha cull floor, w unused.
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var vol_tex: texture_3d<f32>;
@group(0) @binding(2) var vol_samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    // Oversized triangle covering the NDC square [-1, 1]^2.
    var xy = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let p = xy[vid];
    var out: VsOut;
    out.clip = vec4<f32>(p, 0.0, 1.0);
    out.ndc = p;
    return out;
}

// Ray / AABB slab test. Returns (t_near, t_far); a miss has t_far < t_near.
fn intersect_box(ro: vec3<f32>, rd: vec3<f32>, bmin: vec3<f32>, bmax: vec3<f32>) -> vec2<f32> {
    let inv = 1.0 / rd;
    let t0 = (bmin - ro) * inv;
    let t1 = (bmax - ro) * inv;
    let tsmall = min(t0, t1);
    let tbig = max(t0, t1);
    let tnear = max(max(tsmall.x, tsmall.y), tsmall.z);
    let tfar = min(min(tbig.x, tbig.y), tbig.z);
    return vec2<f32>(tnear, tfar);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Un-project the near/far NDC points to build the world-space ray.
    let near_h = u.inv_mvp * vec4<f32>(in.ndc, -1.0, 1.0);
    let far_h = u.inv_mvp * vec4<f32>(in.ndc, 1.0, 1.0);
    let near_w = near_h.xyz / near_h.w;
    let far_w = far_h.xyz / far_h.w;
    let ro = near_w;
    let rd = normalize(far_w - near_w);

    let hit = intersect_box(ro, rd, u.vol_min.xyz, u.vol_max.xyz);
    let tnear = max(hit.x, 0.0);
    let tfar = hit.y;
    if tfar <= tnear {
        discard;
    }

    let steps = max(i32(u.params.x), 1);
    let alpha_scale = u.params.y;
    let cull_floor = u.params.z;
    let extent = u.vol_max.xyz - u.vol_min.xyz;
    let dt = (tfar - tnear) / f32(steps);

    var acc = vec4<f32>(0.0, 0.0, 0.0, 0.0); // premultiplied rgb + alpha
    var t = tnear + dt * 0.5;
    for (var i = 0; i < steps; i = i + 1) {
        let p = ro + rd * t;
        let uvw = (p - u.vol_min.xyz) / extent;
        let s = textureSampleLevel(vol_tex, vol_samp, uvw, 0.0);
        if s.a > cull_floor {
            let a = clamp(s.a * alpha_scale, 0.0, 1.0);
            let w = (1.0 - acc.a) * a;
            acc = vec4<f32>(acc.rgb + s.rgb * w, acc.a + w);
        }
        if acc.a >= 0.995 {
            break;
        }
        t = t + dt;
    }
    return acc; // premultiplied, for PREMULTIPLIED_ALPHA_BLENDING
}
