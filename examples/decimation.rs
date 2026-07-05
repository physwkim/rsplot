//! Decimation example: a curve with far more points than screen pixels.
//!
//! The curve carries 500k vertices. Each frame the widget re-decimates it to a
//! per-pixel-column min/max envelope for the current view, so the GPU draws only
//! a few thousand vertices while the noise band still looks identical. Box-zoom
//! into a region and the envelope is recomputed for the tighter window — zoom in
//! far enough and every original point becomes visible (`doc/design.md` §13 D1).
//!
//! Run with: `cargo run --release --example decimation`

use eframe::egui;
use egui::Color32;
use rsplot::{CurveData, Plot, PlotView, install, set_curve};

const N: usize = 500_000;

fn build_points() -> (Vec<f64>, Vec<f64>) {
    // A slow sine plus a deterministic high-frequency "noise" term, so a pixel
    // column spans a visible vertical band that min/max decimation must keep.
    let x: Vec<f64> = (0..N).map(|i| i as f64 / (N - 1) as f64 * 100.0).collect();
    let y: Vec<f64> = x
        .iter()
        .map(|&t| {
            let base = (t * 0.4).sin() * 3.0;
            let noise = (t * 53.7).sin() * 0.6 + (t * 191.3).cos() * 0.4;
            base + noise
        })
        .collect();
    (x, y)
}

struct DecimationApp {
    plot: Plot,
}

impl DecimationApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        let (x, y) = build_points();
        let curve = CurveData::new(x, y, Color32::from_rgb(120, 200, 160)).with_width(1.0);
        set_curve(render_state, 0, &curve);

        let mut plot = Plot::new(0);
        plot.limits = (0.0, 100.0, -5.0, 5.0);

        Self { plot }
    }
}

impl eframe::App for DecimationApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.label(format!(
                "{N} source points — left-drag to box-zoom, double-click to reset"
            ));
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
        "rsplot · decimation",
        options,
        Box::new(|cc| Ok(Box::new(DecimationApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
