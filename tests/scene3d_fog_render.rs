//! Headless GPU checks for the R1-23 viewport shading port: linear fog
//! (silx `scene/function.py Fog`, `viewport.py:227-233`) and the specular term
//! gated on shininess (`function.py:263-275`, `ScalarFieldView.py:928`).
//!
//! The naga tests in `render/shaders.rs` prove the WGSL parses/validates; these
//! prove the terms actually change pixels, via the synchronous
//! `snapshot_scene3d_with` readback (no egui harness needed).

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use rsplot::egui::Color32;
use rsplot::{
    Camera, Scene3dFog, Scene3dGeometry, Scene3dShading, Vec3, install_scene3d, set_scene3d,
    snapshot_scene3d, snapshot_scene3d_with,
};

const SIZE: (u32, u32) = (100, 100);

/// Camera at (0,0,5) looking down -z — the pose used across the render tests.
fn camera() -> Camera {
    Camera::new(
        30.0,
        0.1,
        100.0,
        (SIZE.0 as f32, SIZE.1 as f32),
        Vec3::new(0.0, 0.0, 5.0),
        Vec3::new(0.0, 0.0, -1.0),
        Vec3::new(0.0, 1.0, 0.0),
    )
}

fn centre_pixel(rgba: &[u8]) -> (u8, u8, u8) {
    let (w, h) = (SIZE.0 as usize, SIZE.1 as usize);
    let i = ((h / 2) * w + w / 2) * 4;
    (rgba[i], rgba[i + 1], rgba[i + 2])
}

#[test]
fn linear_fog_fades_far_geometry_toward_background() {
    let rs = create_render_state(default_wgpu_setup());
    install_scene3d(&rs);

    // A red triangle at the FAR side of the unit-cube bounds (z = -1; camera z
    // = -6 = the fog far end → factor 0.9) covering screen centre.
    let mut g = Scene3dGeometry::new();
    g.add_triangle(
        [-0.5, -0.5, -1.0],
        [0.5, -0.5, -1.0],
        [0.0, 0.5, -1.0],
        Color32::from_rgb(255, 0, 0),
    );
    set_scene3d(&rs, 7, &g);

    let cam = camera();
    let bounds = (Vec3::new(-1.0, -1.0, -1.0), Vec3::new(1.0, 1.0, 1.0));

    // Fog OFF: centre is fully red.
    let plain = snapshot_scene3d(&rs, 7, &cam, Color32::BLACK, SIZE).expect("snapshot");
    let (r0, g0, b0) = centre_pixel(&plain);
    assert!(
        r0 > 200 && g0 < 40 && b0 < 40,
        "no-fog centre should be red, got ({r0},{g0},{b0})"
    );

    // Fog LINEAR toward a black background: factor 0.9 at the far plane, so the
    // red is reduced to ~10% intensity.
    let shading = Scene3dShading {
        fog: Some(Scene3dFog::linear(&cam, bounds, Color32::BLACK)),
        shininess: 0.0,
    };
    let fogged =
        snapshot_scene3d_with(&rs, 7, &cam, Color32::BLACK, SIZE, shading, None).expect("snapshot");
    let (r1, g1, b1) = centre_pixel(&fogged);
    assert!(
        r1 < r0 / 2,
        "fogged far triangle should fade toward black: {r1} !< {}",
        r0 / 2
    );
    assert!(
        r1 > 0,
        "fog factor is 0.9 (not 1.0) at the far end — some red must remain"
    );
    assert!(g1 < 40 && b1 < 40, "fog toward black adds no green/blue");
}

#[test]
fn specular_term_activates_with_shininess() {
    let rs = create_render_state(default_wgpu_setup());
    install_scene3d(&rs);

    // A dark lit mesh facing the camera: normal (0,0,1) reflects the headlight
    // (0,0,-1) straight back at the viewer → specular factor ≈ 1 at centre.
    let mut g = Scene3dGeometry::new();
    g.add_mesh_triangle(
        [[-0.5, -0.5, 0.0], [0.5, -0.5, 0.0], [0.0, 0.5, 0.0]],
        Color32::from_rgb(50, 50, 50),
        [[0.0, 0.0, 1.0]; 3],
    );
    set_scene3d(&rs, 8, &g);
    let cam = camera();

    // shininess 0 (SceneWidget default): no specular — centre stays dark.
    let flat = snapshot_scene3d(&rs, 8, &cam, Color32::BLACK, SIZE).expect("snapshot");
    let (r0, _, _) = centre_pixel(&flat);
    assert!(r0 < 90, "no-specular dark mesh stays dark, got r = {r0}");

    // shininess 32 (ScalarFieldView, ScalarFieldView.py:928): a white specular
    // highlight saturates the centre.
    let shading = Scene3dShading {
        fog: None,
        shininess: 32.0,
    };
    let shiny =
        snapshot_scene3d_with(&rs, 8, &cam, Color32::BLACK, SIZE, shading, None).expect("snapshot");
    let (r1, g1, b1) = centre_pixel(&shiny);
    assert!(
        r1 > 200 && g1 > 200 && b1 > 200,
        "specular highlight should saturate the centre, got ({r1},{g1},{b1})"
    );
}
