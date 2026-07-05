//! Per-vertex curve color: a single polyline whose color varies along its
//! length (silx per-point line color). Each vertex carries its own RGBA and the
//! segment between two vertices is a gradient between their colors.
//!
//! Here the hue sweeps through a perceptual rainbow (colorous Turbo) from the
//! left end to the right, so the curve reads as a continuous spectrum.
//!
//! Run with: `cargo run --example per_vertex_color`

use eframe::egui;
use egui::Color32;
use rsplot::{CurveData, Plot, PlotView, install, set_curve};

const T_MAX: f64 = std::f64::consts::TAU;

fn build_curve() -> CurveData {
    let n = 400;
    let x: Vec<f64> = (0..n).map(|i| i as f64 / (n - 1) as f64 * T_MAX).collect();
    let y: Vec<f64> = x.iter().map(|&t| (2.0 * t).sin()).collect();
    // One color per vertex: Turbo gradient swept along the x fraction.
    let colors: Vec<Color32> = (0..n)
        .map(|i| {
            let c = colorous::TURBO.eval_continuous(i as f64 / (n - 1) as f64);
            Color32::from_rgb(c.r, c.g, c.b)
        })
        .collect();
    // The single `color` (white) is ignored once per-vertex colors are set; it
    // is only the fallback when `colors` is `None`.
    CurveData::new(x, y, Color32::WHITE)
        .with_width(3.0)
        .with_colors(colors)
}

struct PerVertexColorApp {
    plot: Plot,
}

impl PerVertexColorApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        set_curve(render_state, 0, &build_curve());

        let mut plot = Plot::new(0);
        plot.limits = (0.0, T_MAX, -1.2, 1.2);
        plot.title = Some("per-vertex color (Turbo)".to_owned());
        plot.x_label = Some("t".to_owned());
        plot.y_label = Some("sin(2t)".to_owned());

        Self { plot }
    }
}

impl eframe::App for PerVertexColorApp {
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
        "rsplot · per_vertex_color",
        options,
        Box::new(|cc| Ok(Box::new(PerVertexColorApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
