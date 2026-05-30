//! Error bars example.
//!
//! Mirrors silx `examples/pygfx_backend/06_error_bars.py`: three curves
//! with different error bar configurations — symmetric Y, per-point Y,
//! and asymmetric X + symmetric Y. The bars and end caps remain at a
//! fixed pixel size when panning/zooming.
//!
//! High-level API: `CurveData::with_y_error` / `with_x_error` accept an
//! `ErrorBars` enum variant and are forwarded directly by `Plot1D`.
//!
//! Run with: `cargo run --example high_level_error_bars`

use eframe::egui;
use egui_silx::{CurveData, ErrorBars, Plot1D, Symbol};

struct ErrorBarsApp {
    plot: Plot1D,
}

impl ErrorBarsApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let mut plot = Plot1D::new(rs, 0);
        plot.set_graph_title("Error bars");
        plot.set_graph_x_label("x");

        let x: Vec<f64> = (0..9).map(|i| i as f64).collect();

        // Curve 1: symmetric ±0.4 Y error on every point.
        let y0: Vec<f64> = x.iter().map(|&t| (t * 0.6).sin() * 1.5 + 6.0).collect();
        let c0 = CurveData::new(x.clone(), y0, egui::Color32::from_rgb(90, 160, 255))
            .with_width(1.5)
            .with_symbol(Symbol::Circle)
            .with_marker_size(6.0)
            .with_y_error(ErrorBars::Symmetric(0.4));
        let h0 = plot.add_curve_data(&c0);
        plot.set_item_legend(h0, "symmetric Y error");

        // Curve 2: per-point Y error growing with x.
        let y1: Vec<f64> = x.iter().map(|&t| (t * 0.6).cos() * 1.2 + 3.0).collect();
        let err1: Vec<f64> = x.iter().map(|&t| 0.1 + t * 0.08).collect();
        let c1 = CurveData::new(x.clone(), y1, egui::Color32::from_rgb(255, 170, 90))
            .with_width(1.5)
            .with_symbol(Symbol::Square)
            .with_marker_size(6.0)
            .with_y_error(ErrorBars::PerPoint(err1));
        let h1 = plot.add_curve_data(&c1);
        plot.set_item_legend(h1, "per-point Y error");

        // Curve 3: asymmetric X error + symmetric Y error.
        let y2: Vec<f64> = x.iter().map(|&t| (t * 0.6).sin() * 0.8 + 0.5).collect();
        let lower: Vec<f64> = x.iter().map(|&t| 0.15 + t * 0.04).collect();
        let upper: Vec<f64> = x.iter().map(|_| 0.5).collect();
        let c2 = CurveData::new(x, y2, egui::Color32::from_rgb(160, 255, 140))
            .with_width(1.5)
            .with_symbol(Symbol::Cross)
            .with_marker_size(7.0)
            .with_x_error(ErrorBars::Asymmetric { lower, upper })
            .with_y_error(ErrorBars::Symmetric(0.3));
        let h2 = plot.add_curve_data(&c2);
        plot.set_item_legend(h2, "asymmetric X + Y error");

        Self { plot }
    }
}

impl eframe::App for ErrorBarsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.plot.show(ui);
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "egui-silx: error bars",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(ErrorBarsApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
