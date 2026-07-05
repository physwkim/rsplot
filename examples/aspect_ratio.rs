//! Aspect-ratio lock example: a unit circle that stays circular.
//!
//! The curve is a parametric circle `(cos t, sin t)` with equal data limits on
//! both axes. With `keep_aspect = true` the widget expands the tighter axis'
//! display range so data units are square on screen, so the circle renders as a
//! circle in any non-square window (silx `setKeepDataAspectRatio`,
//! `doc/design.md` §13 A4). Toggle the checkbox to drop the aspect lock and
//! watch the circle squash to an ellipse when the window is not square.
//!
//! Run with: `cargo run --example aspect_ratio`

use eframe::egui;
use rsplot::{CurveData, Plot, PlotView, install, set_curve};

fn build_circle() -> CurveData {
    let n = 256usize;
    let mut x = Vec::with_capacity(n);
    let mut y = Vec::with_capacity(n);
    for i in 0..n {
        let t = std::f64::consts::TAU * i as f64 / (n - 1) as f64;
        x.push(t.cos());
        y.push(t.sin());
    }
    CurveData::new(x, y, egui::Color32::from_rgb(255, 200, 80))
}

struct AspectApp {
    plot: Plot,
}

impl AspectApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        set_curve(render_state, 0, &build_circle());

        let mut plot = Plot::new(0);
        // Equal data limits on both axes; the aspect lock keeps pixels square.
        plot.limits = (-1.5, 1.5, -1.5, 1.5);
        plot.keep_aspect = true;

        Self { plot }
    }
}

impl eframe::App for AspectApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.checkbox(
                &mut self.plot.keep_aspect,
                "Keep data aspect ratio (square pixels)",
            );
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
        "rsplot · aspect ratio",
        options,
        Box::new(|cc| Ok(Box::new(AspectApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
