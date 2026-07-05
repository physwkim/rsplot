//! Filled curves: the area between a curve and a baseline is filled with the
//! curve color (silx `fill` + `baseline`).
//!
//! Three curves show the baseline variants: a translucent fill down to y = 0,
//! a fill down to a non-zero scalar baseline, and a band between a curve and a
//! per-point baseline (a filled-between region). The stroke is drawn on top of
//! its own fill.
//!
//! Run with: `cargo run --example fill`

use eframe::egui;
use egui::Color32;
use rsplot::{Baseline, CurveData, Plot, PlotView, install, set_curves};

const T_MAX: f64 = std::f64::consts::TAU;

fn xs(n: usize) -> Vec<f64> {
    (0..n).map(|i| i as f64 / (n - 1) as f64 * T_MAX).collect()
}

fn build() -> Vec<CurveData> {
    let x = xs(300);

    // Fill to y = 0 with a translucent blue.
    let y0: Vec<f64> = x.iter().map(|&t| t.sin() * 0.9 + 1.5).collect();
    let fill0 = CurveData::new(
        x.clone(),
        y0,
        Color32::from_rgba_unmultiplied(90, 160, 255, 90),
    )
    .with_width(2.0)
    .with_fill(Baseline::Scalar(0.0));

    // Fill down to a non-zero scalar baseline.
    let y1: Vec<f64> = x.iter().map(|&t| (2.0 * t).cos() * 0.6 - 1.5).collect();
    let fill1 = CurveData::new(
        x.clone(),
        y1,
        Color32::from_rgba_unmultiplied(255, 170, 90, 90),
    )
    .with_width(2.0)
    .with_fill(Baseline::Scalar(-3.0));

    // Band between a curve and a per-point baseline (filled-between).
    let upper: Vec<f64> = x.iter().map(|&t| t.sin() * 0.4 + 4.0).collect();
    let lower: Vec<f64> = x.iter().map(|&t| t.sin() * 0.4 + 3.4).collect();
    let band = CurveData::new(
        x,
        upper,
        Color32::from_rgba_unmultiplied(160, 255, 140, 110),
    )
    .with_width(1.5)
    .with_fill(Baseline::PerPoint(lower));

    vec![fill0, fill1, band]
}

struct FillApp {
    plot: Plot,
}

impl FillApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);
        set_curves(render_state, 0, &build());

        let mut plot = Plot::new(0);
        plot.limits = (0.0, T_MAX, -4.0, 5.0);
        plot.title = Some("filled curves".to_owned());

        Self { plot }
    }
}

impl eframe::App for FillApp {
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
        "rsplot · fill",
        options,
        Box::new(|cc| Ok(Box::new(FillApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
