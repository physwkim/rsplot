//! Error bars: per-point x/y uncertainty drawn as a bar with end caps (silx
//! `xerror` / `yerror`).
//!
//! Three curves show the error variants: a symmetric y error on every point, a
//! per-point y error that grows with x, and asymmetric x+y error bars. The bar
//! and its caps keep a fixed pixel size at any zoom; the curve line and markers
//! draw on top of the bars.
//!
//! Run with: `cargo run --example error_bars`

use eframe::egui;
use egui::Color32;
use rsplot::{CurveData, ErrorBars, Plot, PlotView, Symbol, install, set_curves};

fn build() -> Vec<CurveData> {
    let x: Vec<f64> = (0..9).map(|i| i as f64).collect();

    // Symmetric y error (same +/- on every point).
    let y0: Vec<f64> = x.iter().map(|&t| (t * 0.6).sin() * 1.5 + 6.0).collect();
    let c0 = CurveData::new(x.clone(), y0, Color32::from_rgb(90, 160, 255))
        .with_width(1.5)
        .with_symbol(Symbol::Circle)
        .with_marker_size(6.0)
        .with_y_error(ErrorBars::Symmetric(0.4));

    // Per-point y error that grows with x.
    let y1: Vec<f64> = x.iter().map(|&t| (t * 0.6).cos() * 1.2 + 3.0).collect();
    let err1: Vec<f64> = x.iter().map(|&t| 0.1 + t * 0.08).collect();
    let c1 = CurveData::new(x.clone(), y1, Color32::from_rgb(255, 170, 90))
        .with_width(1.5)
        .with_symbol(Symbol::Square)
        .with_marker_size(6.0)
        .with_y_error(ErrorBars::PerPoint(err1));

    // Asymmetric x error plus a symmetric y error.
    let y2: Vec<f64> = x.iter().map(|&t| (t * 0.6).sin() * 0.8 + 0.5).collect();
    let lower: Vec<f64> = x.iter().map(|&t| 0.15 + t * 0.04).collect();
    let upper: Vec<f64> = x.iter().map(|_| 0.5).collect();
    let c2 = CurveData::new(x, y2, Color32::from_rgb(160, 255, 140))
        .with_width(1.5)
        .with_symbol(Symbol::Cross)
        .with_marker_size(7.0)
        .with_x_error(ErrorBars::Asymmetric { lower, upper })
        .with_y_error(ErrorBars::Symmetric(0.3));

    vec![c0, c1, c2]
}

struct ErrorBarsApp {
    plot: Plot,
}

impl ErrorBarsApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);
        set_curves(render_state, 0, &build());

        let mut plot = Plot::new(0);
        plot.limits = (-1.0, 9.0, -1.0, 9.0);
        plot.title = Some("error bars".to_owned());

        Self { plot }
    }
}

impl eframe::App for ErrorBarsApp {
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
        "rsplot · error_bars",
        options,
        Box::new(|cc| Ok(Box::new(ErrorBarsApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
