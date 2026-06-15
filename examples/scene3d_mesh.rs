//! 3D mesh & cylindrical-volume example.
//!
//! Mirrors the mesh demo of silx plot3d (pygfx `16_3d_mesh`) and exercises the
//! P1.2 items together: a [`ColormapMesh3D`] ripple surface coloured per-vertex
//! by height, plus the three `_CylindricalVolume` primitives —
//! [`Box3D`], [`Cylinder3D`], [`Hexagon3D`] — placed in a row above it. All are
//! lit by silx's camera-fixed headlight.
//!
//! Left-drag orbits, right-drag pans, wheel zooms.
//!
//! Run with: `cargo run --example scene3d_mesh`

use eframe::egui;
use siplot::egui::Color32;
use siplot::{
    Box3D, Colormap, ColormapMesh3D, ColormapName, Cylinder3D, Hexagon3D, MeshDrawMode,
    Scene3dGeometry, SceneWidget, Vec3,
};

struct MeshApp {
    scene: SceneWidget,
}

impl MeshApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let mut geometry = Scene3dGeometry::new();
        ripple_surface().append_to(&mut geometry);
        cylindrical_volumes(&mut geometry);

        let mut scene = SceneWidget::new(rs, 0);
        scene.set_bounds(rs, (Vec3::new(-4.0, -4.0, -1.5), Vec3::new(4.0, 4.0, 3.0)));
        scene.set_geometry(rs, geometry);
        Self { scene }
    }
}

impl eframe::App for MeshApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.scene.show(ui);
        });
    }
}

/// A radial ripple `z = cos(r) / (1 + r)` over `[-4, 4]²`, as a per-vertex
/// colormapped triangle mesh (value = height, viridis).
fn ripple_surface() -> ColormapMesh3D {
    const G: usize = 60;
    let span = 4.0f32;
    let at = |ix: usize, iy: usize| -> ([f32; 3], f64) {
        let u = -span + 2.0 * span * ix as f32 / G as f32;
        let v = -span + 2.0 * span * iy as f32 / G as f32;
        let r = (u * u + v * v).sqrt();
        let z = (r * 2.0).cos() / (1.0 + r);
        ([u, v, z], z as f64)
    };
    let (mut positions, mut values) = (Vec::new(), Vec::new());
    let mut push = |p: ([f32; 3], f64)| {
        positions.push(p.0);
        values.push(p.1);
    };
    for iy in 0..G {
        for ix in 0..G {
            push(at(ix, iy));
            push(at(ix + 1, iy));
            push(at(ix + 1, iy + 1));
            push(at(ix, iy));
            push(at(ix + 1, iy + 1));
            push(at(ix, iy + 1));
        }
    }
    ColormapMesh3D::new()
        .with_colormap(Colormap::new(ColormapName::Viridis, -0.5, 1.0))
        .with_data(&positions, &values, None, MeshDrawMode::Triangles, None)
}

/// A box, a cylinder, and a hexagonal prism standing in a row at `z ≈ 2`.
fn cylindrical_volumes(geometry: &mut Scene3dGeometry) {
    let no_rotation = (0.0, [0.0, 0.0, 1.0]);

    let mut cube = Box3D::new();
    cube.set_data(
        [1.2, 1.2, 1.2],
        &[Color32::from_rgb(220, 90, 90)],
        &[[-2.5, 0.0, 2.0]],
        no_rotation,
    );
    cube.append_to(geometry);

    let mut cyl = Cylinder3D::new();
    cyl.set_data(
        0.7,
        1.4,
        &[Color32::from_rgb(90, 200, 130)],
        48,
        &[[0.0, 0.0, 2.0]],
        no_rotation,
    );
    cyl.append_to(geometry);

    let mut hex = Hexagon3D::new();
    hex.set_data(
        0.8,
        1.4,
        &[Color32::from_rgb(120, 130, 230)],
        &[[2.5, 0.0, 2.0]],
        no_rotation,
    );
    hex.append_to(geometry);
}

fn main() -> eframe::Result {
    eframe::run_native(
        "siplot: 3D mesh & volumes",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(MeshApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
