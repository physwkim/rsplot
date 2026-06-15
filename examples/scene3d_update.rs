//! Live-updating 3D scatter example.
//!
//! The siplot analogue of silx `examples/plot3dUpdateScatterFromThread.py`: a 3D
//! scatter whose geometry is rebuilt every frame (here a double helix spun about
//! the vertical axis), re-uploaded through [`SceneWidget::set_geometry`] and kept
//! animating with `ctx.request_repaint()`. silx pushes new data from a worker
//! thread; egui's immediate-mode loop rebuilds in `ui` instead — the data path
//! (`set_geometry` re-upload) is the same.
//!
//! Left-drag orbits, right-drag pans, wheel zooms while it spins.
//!
//! Run with: `cargo run --example scene3d_update`

use eframe::egui;
use siplot::egui_wgpu::RenderState;
use siplot::{Colormap, PointMarker, Scatter3D, Scene3dGeometry, SceneWidget, Vec3};

const N: usize = 300;

struct UpdateApp {
    scene: SceneWidget,
    rs: RenderState,
}

impl UpdateApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");
        let mut scene = SceneWidget::new(rs, 0);
        scene.set_bounds(rs, (Vec3::new(-1.2, -1.2, -1.2), Vec3::new(1.2, 1.2, 1.2)));
        Self {
            scene,
            rs: rs.clone(),
        }
    }

    /// Rebuild the helix scatter rotated by `angle` (radians) about the Z axis.
    fn rebuild(&mut self, angle: f32) {
        let (mut xs, mut ys, mut zs, mut vs) = (vec![], vec![], vec![], vec![]);
        for i in 0..N {
            let t = i as f32 / (N - 1) as f32;
            // Double helix: two strands a half-turn apart.
            let phase = t * 6.0 * std::f32::consts::TAU + angle;
            let strand = if i % 2 == 0 {
                0.0
            } else {
                std::f32::consts::PI
            };
            xs.push((phase + strand).cos());
            ys.push((phase + strand).sin());
            zs.push(t * 2.0 - 1.0);
            vs.push(t as f64);
        }
        let scatter = Scatter3D::new()
            .with_data(&xs, &ys, &zs, &vs)
            .with_colormap(Colormap::viridis(0.0, 1.0))
            .with_marker(PointMarker::Circle)
            .with_size(8.0);
        let mut geometry = Scene3dGeometry::new();
        scatter.append_to(&mut geometry);
        self.scene.set_geometry(&self.rs, geometry);
    }
}

impl eframe::App for UpdateApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // egui's monotonic clock drives the animation (no wall-clock needed).
        let angle = ui.input(|i| i.time) as f32 * 0.6;
        self.rebuild(angle);
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.scene.show(ui);
        });
        ui.ctx().request_repaint(); // keep the animation running
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "siplot: live 3D scatter",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(UpdateApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
