//! Markers example: the five SDF symbols drawn at curve vertices.
//!
//! Five sparse curves are stacked, each using one symbol (circle, square,
//! cross, plus, triangle) at 14 px, with a thin connecting line. The vertex
//! shader places one screen-space quad per point and the fragment shader fills
//! the symbol's region, so marker size is in pixels regardless of zoom
//! (`doc/design.md` §13 B2).
//!
//! Run with: `cargo run --example markers`

use eframe::egui;
use rsplot::{CurveData, Plot, PlotView, Symbol, install, set_curves};

const N: usize = 12;

fn row(offset: f64, symbol: Symbol, color: egui::Color32) -> CurveData {
    let mut x = Vec::with_capacity(N);
    let mut y = Vec::with_capacity(N);
    for i in 0..N {
        let t = i as f64 / (N - 1) as f64; // 0..1
        x.push(t * 10.0);
        y.push(offset + 0.6 * (t * std::f64::consts::TAU).sin());
    }
    CurveData::new(x, y, color)
        .with_width(1.5)
        .with_symbol(symbol)
        .with_marker_size(14.0)
}

struct MarkersApp {
    plot: Plot,
}

impl MarkersApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        let curves = [
            row(1.0, Symbol::Circle, egui::Color32::from_rgb(120, 180, 255)),
            row(2.0, Symbol::Square, egui::Color32::from_rgb(120, 255, 180)),
            row(3.0, Symbol::Cross, egui::Color32::from_rgb(255, 220, 120)),
            row(4.0, Symbol::Plus, egui::Color32::from_rgb(255, 150, 200)),
            row(
                5.0,
                Symbol::Triangle,
                egui::Color32::from_rgb(255, 120, 120),
            ),
        ];
        set_curves(render_state, 0, &curves);

        let mut plot = Plot::new(0);
        plot.limits = (-0.5, 10.5, 0.0, 6.0);

        Self { plot }
    }
}

impl eframe::App for MarkersApp {
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
        "rsplot · markers",
        options,
        Box::new(|cc| Ok(Box::new(MarkersApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
