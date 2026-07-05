//! Thick-line example: four curves of increasing pixel width.
//!
//! Each curve is the same sine, offset vertically, drawn at 1, 3, 6, and 10
//! physical pixels via `CurveData::with_width`. The vertex shader expands every
//! segment into a screen-space quad of that width, so the thickness stays
//! uniform regardless of the data aspect ratio or zoom (`doc/design.md` §13 B1).
//!
//! Run with: `cargo run --example thick_lines`

use eframe::egui;
use rsplot::{CurveData, Plot, PlotView, install, set_curves};

const N: usize = 300;

fn sine_at(offset: f64, width: f32, color: egui::Color32) -> CurveData {
    let mut x = Vec::with_capacity(N);
    let mut y = Vec::with_capacity(N);
    for i in 0..N {
        let t = 10.0 * i as f64 / (N - 1) as f64;
        x.push(t);
        y.push(offset + (t * std::f64::consts::TAU / 5.0).sin());
    }
    CurveData::new(x, y, color).with_width(width)
}

struct ThickLinesApp {
    plot: Plot,
}

impl ThickLinesApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        let curves = [
            sine_at(0.0, 1.0, egui::Color32::from_rgb(120, 180, 255)),
            sine_at(3.0, 3.0, egui::Color32::from_rgb(120, 255, 180)),
            sine_at(6.0, 6.0, egui::Color32::from_rgb(255, 220, 120)),
            sine_at(9.0, 10.0, egui::Color32::from_rgb(255, 120, 120)),
        ];
        set_curves(render_state, 0, &curves);

        let mut plot = Plot::new(0);
        plot.limits = (0.0, 10.0, -1.5, 10.5);

        Self { plot }
    }
}

impl eframe::App for ThickLinesApp {
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
        "rsplot · thick lines",
        options,
        Box::new(|cc| Ok(Box::new(ThickLinesApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
