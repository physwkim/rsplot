//! Triangles: a per-vertex-colored filled mesh (silx `addTriangles`).
//!
//! An 8×6 vertex grid over the data area is triangulated into two triangles per
//! cell and colored by position (red across x, green across y, a blue tint), so
//! the Gouraud interpolation across each triangle is visible. A global alpha
//! makes the mesh translucent. The mesh is an egui-painter overlay drawn in the
//! data layer (under the grid/frame), transformed on the CPU so it follows
//! pan/zoom (`doc/design.md` §8).
//!
//! Run with: `cargo run --example triangles`

use eframe::egui;
use rsplot::{Plot, PlotView, Triangles, install};

const NX: usize = 8;
const NY: usize = 6;

fn build_mesh() -> Triangles {
    let mut x = Vec::with_capacity(NX * NY);
    let mut y = Vec::with_capacity(NX * NY);
    let mut colors = Vec::with_capacity(NX * NY);
    for j in 0..NY {
        for i in 0..NX {
            let fx = i as f64 / (NX - 1) as f64; // 0..1
            let fy = j as f64 / (NY - 1) as f64;
            x.push(fx * 10.0);
            y.push(fy * 8.0);
            let r = (255.0 * fx) as u8;
            let g = (255.0 * fy) as u8;
            let b = (255.0 * (1.0 - fx * fy)) as u8;
            colors.push(egui::Color32::from_rgb(r, g, b));
        }
    }
    // Two triangles per grid cell.
    let mut indices = Vec::with_capacity((NX - 1) * (NY - 1) * 2);
    let vid = |i: usize, j: usize| (j * NX + i) as u32;
    for j in 0..NY - 1 {
        for i in 0..NX - 1 {
            indices.push([vid(i, j), vid(i + 1, j), vid(i + 1, j + 1)]);
            indices.push([vid(i, j), vid(i + 1, j + 1), vid(i, j + 1)]);
        }
    }
    Triangles::new(x, y, indices, colors).with_alpha(0.85)
}

struct TriApp {
    plot: Plot,
}

impl TriApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, 0.0, 8.0);
        plot.title = Some("triangles (per-vertex color, alpha 0.85)".to_owned());
        plot.triangles.push(build_mesh());

        Self { plot }
    }
}

impl eframe::App for TriApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            PlotView::new().show(ui, &mut self.plot);
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "rsplot · triangles",
        options,
        Box::new(|cc| Ok(Box::new(TriApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
