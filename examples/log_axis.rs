//! Log-axis example: a power-law curve on a log10 Y axis.
//!
//! `y = 10^(x/12)` over `x ∈ [0, 48]` spans four decades (1 → 10⁴). On a linear
//! axis this is an explosive exponential; with `y_scale = Log10` it renders as a
//! straight line and the Y gutter shows one tick per power of ten. The shader
//! applies log10 in the vertex stage and the chrome draws decade ticks, both
//! derived from the same `Transform`, so the line and its ticks stay aligned
//! while panning and zooming (`doc/design.md` §13 A3).
//!
//! Run with: `cargo run --example log_axis`

use eframe::egui;
use rsplot::{CurveData, Plot, PlotView, Scale, install, set_curve};

fn build_curve() -> CurveData {
    // Power law y = 10^(x/12): a straight line on a log10 Y axis.
    let n = 200usize;
    let mut x = Vec::with_capacity(n);
    let mut y = Vec::with_capacity(n);
    for i in 0..n {
        let xi = 48.0 * i as f64 / (n - 1) as f64; // 0..48
        x.push(xi);
        y.push(10f64.powf(xi / 12.0)); // 1..1e4
    }
    CurveData::new(x, y, egui::Color32::from_rgb(120, 200, 255))
}

struct LogAxisApp {
    plot: Plot,
}

impl LogAxisApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        set_curve(render_state, 0, &build_curve());

        let mut plot = Plot::new(0);
        // Y limits must be strictly positive for a log axis; span the full four
        // decades the curve covers, with a little headroom.
        plot.limits = (0.0, 48.0, 1.0, 1.0e4);
        plot.y_scale = Scale::Log10;

        Self { plot }
    }
}

impl eframe::App for LogAxisApp {
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
        "rsplot · log axis",
        options,
        Box::new(|cc| Ok(Box::new(LogAxisApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
