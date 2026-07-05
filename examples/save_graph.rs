//! save_graph example: render the current plot view to a PNG file.
//!
//! Click "Save PNG" to render the data layer (background + curve) for the
//! current limits into an offscreen target, read it back, and write it to
//! `rsplot-graph.png` in the working directory. Pan/zoom first to change
//! what gets saved (`doc/design.md` §13 E1).
//!
//! Run with: `cargo run --example save_graph`

use eframe::egui;
use egui::Color32;
use rsplot::{CurveData, Plot, PlotView, install, save_graph, set_curve};

const OUT_PATH: &str = "rsplot-graph.png";
const SAVE_SIZE: (u32, u32) = (800, 600);

fn build_points() -> (Vec<f64>, Vec<f64>) {
    let n = 400;
    let x: Vec<f64> = (0..n).map(|i| i as f64 / (n - 1) as f64 * 10.0).collect();
    let y: Vec<f64> = x
        .iter()
        .map(|&t| 5.0 + 3.0 * (t * std::f64::consts::TAU * 0.2).sin())
        .collect();
    (x, y)
}

struct SaveApp {
    plot: Plot,
    status: String,
}

impl SaveApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        let (x, y) = build_points();
        let curve = CurveData::new(x, y, Color32::from_rgb(120, 180, 255)).with_width(2.0);
        set_curve(render_state, 0, &curve);

        let mut plot = Plot::new(0);
        plot.limits = (-0.5, 10.5, 0.0, 9.0);

        Self {
            plot,
            status: format!("Click to save {SAVE_SIZE:?} px → {OUT_PATH}"),
        }
    }
}

impl eframe::App for SaveApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Save PNG").clicked()
                    && let Some(rs) = frame.wgpu_render_state()
                {
                    self.status = match save_graph(rs, &self.plot, SAVE_SIZE, OUT_PATH) {
                        Ok(()) => format!("saved {}×{} → {OUT_PATH}", SAVE_SIZE.0, SAVE_SIZE.1),
                        Err(e) => format!("save failed: {e}"),
                    };
                }
                ui.label(&self.status);
            });
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
        "rsplot · save_graph",
        options,
        Box::new(|cc| Ok(Box::new(SaveApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
