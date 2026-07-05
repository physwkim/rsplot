//! Low-level 3D scene example: a [`SceneWidget`] hosting two data items built
//! from scratch — a colormapped [`Scatter3D`] helix and a lit [`Mesh3D`] saddle
//! surface — beneath the automatic bounding-box + RGB-axes chrome.
//!
//! This is the plot3d analogue of the 2D low-level examples (`markers`,
//! `triangles`): it drives the scene-graph API directly (`Scene3dGeometry` +
//! `append_to` + `set_geometry`) rather than a high-level wrapper. Left-drag to
//! orbit, right-drag to pan, wheel to zoom.
//!
//! Run with: `cargo run --example scene3d`

use eframe::egui;
use rsplot::egui::Color32;
use rsplot::{
    Colormap, Mesh3D, MeshColor, MeshDrawMode, PointMarker, Scatter3D, Scene3dGeometry,
    SceneWidget, Vec3,
};

struct Scene3dApp {
    scene: SceneWidget,
}

impl Scene3dApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        // One geometry buffer collects every item; the widget appends the chrome
        // (box + axes) on top at upload time.
        let mut geometry = Scene3dGeometry::new();
        helix().append_to(&mut geometry);
        saddle().append_to(&mut geometry);

        let mut scene = SceneWidget::new(rs, 0);
        // Frame the camera to the box that encloses both items.
        scene.set_bounds(rs, (Vec3::new(-1.2, -1.2, -1.2), Vec3::new(1.2, 1.2, 1.2)));
        scene.set_geometry(rs, geometry);
        Self { scene }
    }
}

impl eframe::App for Scene3dApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.scene.show(ui);
        });
    }
}

/// A helix of points, coloured by the curve parameter through a colormap.
fn helix() -> Scatter3D {
    const N: usize = 240;
    let (mut xs, mut ys, mut zs, mut vs) = (
        Vec::with_capacity(N),
        Vec::with_capacity(N),
        Vec::with_capacity(N),
        Vec::with_capacity(N),
    );
    let turns = 4.0;
    for i in 0..N {
        let t = i as f32 / (N - 1) as f32; // 0..1
        let angle = t * turns * std::f32::consts::TAU;
        xs.push(angle.cos());
        ys.push(angle.sin());
        zs.push(t * 2.0 - 1.0); // -1..1
        vs.push(t as f64);
    }
    Scatter3D::new()
        .with_data(&xs, &ys, &zs, &vs)
        .with_colormap(Colormap::viridis(0.0, 1.0))
        .with_marker(PointMarker::Circle)
        .with_size(9.0)
}

/// A saddle surface `z = 0.4·(u² − v²)` over a grid, as a lit uniform-colour
/// triangle mesh (flat normals are computed from the triangles).
fn saddle() -> Mesh3D {
    const G: usize = 24;
    let span = 1.1f32;
    let at = |ix: usize, iy: usize| -> [f32; 3] {
        let u = -span + 2.0 * span * ix as f32 / G as f32;
        let v = -span + 2.0 * span * iy as f32 / G as f32;
        [u, v, 0.4 * (u * u - v * v)]
    };
    let mut positions = Vec::with_capacity(G * G * 6);
    for iy in 0..G {
        for ix in 0..G {
            // Two triangles per grid cell.
            positions.push(at(ix, iy));
            positions.push(at(ix + 1, iy));
            positions.push(at(ix + 1, iy + 1));
            positions.push(at(ix, iy));
            positions.push(at(ix + 1, iy + 1));
            positions.push(at(ix, iy + 1));
        }
    }
    Mesh3D::new().with_data(
        &positions,
        MeshColor::Uniform(Color32::from_rgb(90, 170, 200)),
        None, // flat normals computed per triangle
        MeshDrawMode::Triangles,
        None,
    )
}

fn main() -> eframe::Result {
    eframe::run_native(
        "rsplot: 3D scene",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(Scene3dApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
