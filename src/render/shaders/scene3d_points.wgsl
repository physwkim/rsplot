// 3D point-sprite (scatter) shader — the wgpu analogue of silx
// `scene/primitives.py` `_Points`.
//
// wgpu's `PointList` topology only ever rasterises 1×1-pixel points (there is no
// `gl_PointSize`), so a sized, screen-facing point symbol is drawn as a
// billboarded quad: each point is one instance, expanded to two triangles whose
// corners are offset from the projected centre by `size` pixels (converted to an
// NDC offset, multiplied by clip `w` so it survives the perspective divide). The
// fragment stage reproduces silx's per-marker `alphaSymbol(gl_PointCoord, size)`
// coverage functions and discards fully-transparent texels.

struct PointParams {
    // camera.matrix() × model, column-major, depth-corrected (same as scene3d.wgsl).
    mvp: mat4x4<f32>,
    // Linear fog datum (see scene3d.wgsl): x = scale, y = near, z = on/off.
    fog_info: vec4<f32>,
    fog_color: vec4<f32>,
    // Row 2 of the view matrix — camera-space z of the point centre (silx fogs
    // `_Points` on the point's `vCameraPosition`, primitives.py).
    view_row_z: vec4<f32>,
    // Offscreen target size in physical pixels (for the pixel→NDC offset).
    viewport: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> params: PointParams;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    // 0..1 across the sprite, the analogue of GL's gl_PointCoord.
    @location(0) coord: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) marker: u32,
    @location(3) size: f32,
    // Camera-space z of the point centre (shared by the whole sprite).
    @location(4) cam_z: f32,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vi: u32,
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) size: f32,
    @location(3) marker: u32,
) -> VsOut {
    // Two triangles spanning the unit quad, corners in [-0.5, 0.5].
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-0.5, -0.5),
        vec2<f32>(0.5, -0.5),
        vec2<f32>(0.5, 0.5),
        vec2<f32>(-0.5, -0.5),
        vec2<f32>(0.5, 0.5),
        vec2<f32>(-0.5, 0.5),
    );
    let corner = corners[vi];

    let center = params.mvp * vec4<f32>(pos, 1.0);
    // Pixel offset → NDC offset (NDC spans 2.0 over `viewport` pixels), scaled by
    // clip `w` so the size stays constant in pixels after the perspective divide.
    let ndc = vec2<f32>(
        2.0 * corner.x * size / params.viewport.x,
        2.0 * corner.y * size / params.viewport.y,
    );

    var out: VsOut;
    out.clip = vec4<f32>(center.xy + ndc * center.w, center.z, center.w);
    out.coord = corner + vec2<f32>(0.5, 0.5);
    out.color = color;
    out.marker = marker;
    out.size = size;
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

// Per-marker coverage in [0, 1], ported one-for-one from silx
// `_Points._MARKER_FUNCTIONS` (the `size` factor gives a ~1px antialiased edge).
fn alpha_symbol(coord: vec2<f32>, size: f32, marker: u32) -> f32 {
    let centered = abs(coord - vec2<f32>(0.5, 0.5));
    switch marker {
        // 0: circle ('o')
        case 0u: {
            let r = distance(coord, vec2<f32>(0.5, 0.5));
            return clamp(size * (0.5 - r), 0.0, 1.0);
        }
        // 1: diamond ('d')
        case 1u: {
            let f = centered.x + centered.y;
            return clamp(size * (0.5 - f), 0.0, 1.0);
        }
        // 2: square ('s')
        case 2u: {
            return 1.0;
        }
        // 3: plus ('+')
        case 3u: {
            let d = abs(size * (coord - vec2<f32>(0.5, 0.5)));
            if (min(d.x, d.y) < 0.5) { return 1.0; }
            return 0.0;
        }
        // 4: x-cross ('x')
        case 4u: {
            let p = floor(size * coord) + vec2<f32>(0.5, 0.5);
            let dx = abs(vec2<f32>(p.x - p.y, p.x + p.y - size));
            if (min(dx.x, dx.y) <= 0.5) { return 1.0; }
            return 0.0;
        }
        // 5: asterisk ('*') — combining +, x and a soft circle edge
        case 5u: {
            let dplus = abs(size * (coord - vec2<f32>(0.5, 0.5)));
            let p = floor(size * coord) + vec2<f32>(0.5, 0.5);
            let dx = abs(vec2<f32>(p.x - p.y, p.x + p.y - size));
            if (min(dplus.x, dplus.y) < 0.5) {
                return 1.0;
            } else if (min(dx.x, dx.y) <= 0.5) {
                let r = distance(coord, vec2<f32>(0.5, 0.5));
                return clamp(size * (0.5 - r), 0.0, 1.0);
            }
            return 0.0;
        }
        // 6: horizontal line ('_')
        case 6u: {
            let dy = abs(size * (coord.y - 0.5));
            if (dy < 0.5) { return 1.0; }
            return 0.0;
        }
        // 7: vertical line ('|')
        case 7u: {
            let dx2 = abs(size * (coord.x - 0.5));
            if (dx2 < 0.5) { return 1.0; }
            return 0.0;
        }
        default: {
            return 1.0;
        }
    }
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let a = alpha_symbol(in.coord, in.size, in.marker);
    if (a == 0.0) {
        discard;
    }
    // `color` is linear, premultiplied alpha; scaling every channel by the
    // coverage keeps it premultiplied for the One/OneMinusSrcAlpha blend.
    return apply_fog(in.color * a, in.cam_z);
}
