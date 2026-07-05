//! Headless GPU checks for the R2-47 scene blending port: silx enables
//! `GL_BLEND` (`SRC_ALPHA, ONE_MINUS_SRC_ALPHA`) for the whole scene
//! (`viewport.py:356-357`), so translucent lines/triangles and lit meshes
//! composite over what is behind them. rsplot's line/triangle and mesh pipelines
//! were opaque (blend `None`), dropping iso-surface / `Mesh3D` alpha and the
//! 60 %-alpha axis tick lines.
//!
//! These prove the geometry actually composites, via the synchronous
//! `snapshot_scene3d` readback (no egui harness needed): a translucent blue front
//! primitive over a bright-green cleared background must let the green show
//! through. Under an opaque write the green would be fully replaced by blue.

use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
use rsplot::egui::Color32;
use rsplot::{Camera, Scene3dGeometry, Vec3, install_scene3d, set_scene3d, snapshot_scene3d};

const SIZE: (u32, u32) = (100, 100);

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

/// A triangle covering the screen centre (same shape the fog test uses).
fn centre_triangle_verts(z: f32) -> [[f32; 3]; 3] {
    [[-0.5, -0.5, z], [0.5, -0.5, z], [0.0, 0.5, z]]
}

#[test]
fn translucent_triangle_composites_over_the_background() {
    let rs = create_render_state(default_wgpu_setup());
    install_scene3d(&rs);

    // A 50 %-alpha blue triangle over screen centre.
    let mut g = Scene3dGeometry::new();
    let [a, b, c] = centre_triangle_verts(0.0);
    g.add_triangle(a, b, c, Color32::from_rgba_unmultiplied(0, 0, 255, 128));
    set_scene3d(&rs, 47, &g);

    let cam = camera();
    let bg = Color32::from_rgb(0, 200, 0); // bright green backdrop
    let px = snapshot_scene3d(&rs, 47, &cam, bg, SIZE).expect("snapshot");
    let (r, gc, bl) = centre_pixel(&px);

    // After R2-47 the blue triangle blends over green: the centre is a blue/green
    // composite — green shows through (an opaque write would give pure blue, g≈0).
    assert!(
        gc > 60,
        "translucent triangle must let the green backdrop show through, got g={gc}"
    );
    assert!(
        bl > 100,
        "the blue triangle must still contribute, got b={bl}"
    );
    assert!(r < 40, "no red in a blue-over-green composite, got r={r}");
}

#[test]
fn translucent_mesh_composites_over_the_background() {
    let rs = create_render_state(default_wgpu_setup());
    install_scene3d(&rs);

    // A 50 %-alpha blue lit mesh facing the camera over screen centre.
    let mut g = Scene3dGeometry::new();
    g.add_mesh_triangle(
        centre_triangle_verts(0.0),
        Color32::from_rgba_unmultiplied(0, 0, 255, 128),
        [[0.0, 0.0, 1.0]; 3],
    );
    set_scene3d(&rs, 48, &g);

    let cam = camera();
    let bg = Color32::from_rgb(0, 200, 0);
    let px = snapshot_scene3d(&rs, 48, &cam, bg, SIZE).expect("snapshot");
    let (r, gc, bl) = centre_pixel(&px);

    // The mesh pipeline now blends too: the green backdrop shows through the
    // half-transparent surface (an opaque write would fully replace it).
    assert!(
        gc > 60,
        "translucent mesh must let the green backdrop show through, got g={gc}"
    );
    assert!(bl > 60, "the blue mesh must still contribute, got b={bl}");
    assert!(r < 40, "no red in a blue-over-green composite, got r={r}");
}
