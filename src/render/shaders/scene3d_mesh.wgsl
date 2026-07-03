// Shaded triangle-mesh shader — the wgpu analogue of silx
// `scene/primitives.py` `Mesh3D` lit by `scene/function.py` `DirectionalLight`.
//
// silx shades plot3d meshes with a *headlight*: a directional Phong light fixed
// in camera space pointing into the screen (direction (0,0,-1)), ambient 0.3,
// diffuse 0.7, specular (1,1,1) gated on a per-viewport shininess — 0 (off) for
// `Plot3DWidget`/`SceneWidget`, 32 for `ScalarFieldView`
// (`ScalarFieldView.py:928` `viewport.light.shininess = 32`). The shading
// follows the camera as the scene is orbited, so it must be computed per-frame
// on the GPU. The normal is carried into camera space by the view matrix
// (`normal_mat`); positions are projected by the usual clip MVP.

struct MeshParams {
    // Clip-space MVP (proj × view × model), depth-corrected (as scene3d.wgsl).
    mvp: mat4x4<f32>,
    // Camera-space transform: the view matrix (model is identity — items bake
    // world-space vertices). For normals (w = 0, translation dropped) the rigid
    // view's 3×3 is its own inverse-transpose; for positions (w = 1) it yields
    // the camera-space vertex used by the specular and fog terms.
    normal_mat: mat4x4<f32>,
    // Linear fog datum (see scene3d.wgsl): x = scale, y = near, z = on/off.
    fog_info: vec4<f32>,
    fog_color: vec4<f32>,
    // x = Phong shininess exponent (0 disables specular, the silx
    // DirectionalLight default, function.py:296-300); yzw unused.
    light: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: MeshParams;

// silx viewport DirectionalLight defaults, camera frame (viewport.py:227-233
// direction (0,0,-1), ambient 0.3, diffuse 0.7; specular defaults to (1,1,1)).
const LIGHT_DIR: vec3<f32> = vec3<f32>(0.0, 0.0, -1.0);
const AMBIENT: f32 = 0.3;
const DIFFUSE: f32 = 0.7;
const SPECULAR: vec3<f32> = vec3<f32>(1.0, 1.0, 1.0);

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) normal_cam: vec3<f32>,
    // Camera-space vertex position: silx's `viewPos - position` view vector
    // (the camera sits at the origin of this frame) and the fog's z datum.
    @location(2) pos_cam: vec3<f32>,
};

@vertex
fn vs_main(
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) normal: vec3<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = params.mvp * vec4<f32>(pos, 1.0);
    out.normal_cam = (params.normal_mat * vec4<f32>(normal, 0.0)).xyz;
    out.pos_cam = (params.normal_mat * vec4<f32>(pos, 1.0)).xyz;
    out.color = color;
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
    let n = normalize(in.normal_cam);
    // One-sided Lambert term (silx `max(0.0, dot(normal, -lightDir))`).
    let n_dot_l = max(0.0, dot(n, -LIGHT_DIR));

    // Specular, gated exactly as silx DirectionalLight (function.py:263-275):
    // shininess > 0 and the face lit. In the camera frame the view position is
    // the origin, so viewDir = normalize(-pos_cam).
    var spec_factor = 0.0;
    if (params.light.x > 0.0 && n_dot_l > 0.0) {
        let reflection = reflect(LIGHT_DIR, n);
        let view_dir = normalize(-in.pos_cam);
        spec_factor = max(0.0, dot(reflection, view_dir));
        if (spec_factor > 0.0) {
            spec_factor = pow(spec_factor, params.light.x);
        }
    }

    // silx: color.rgb * (ambient + diffuse·nDotL) + specular·specFactor, alpha
    // untouched. `color` is linear premultiplied, so the additive specular term
    // is scaled by alpha to keep the result premultiplied.
    let factor = AMBIENT + DIFFUSE * n_dot_l;
    let lit = vec4<f32>(
        in.color.rgb * factor + SPECULAR * spec_factor * in.color.a,
        in.color.a,
    );
    return apply_fog(lit, in.pos_cam.z);
}
