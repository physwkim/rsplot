//! Labels example: graph title, X/Y axis labels, and a foreground color
//! override (silx setGraphTitle / setGraphXLabel / setGraphYLabel /
//! setForegroundColor).
//!
//! The title sits centered above the data area, the X label below the X ticks,
//! and the Y label rotated at the far left. A custom foreground color recolors
//! the frame, ticks, and label text.
//!
//! Run with: `cargo run --example labels`

use eframe::egui;
use egui::Color32;
use rsplot::{CurveData, Plot, PlotView, install, set_curve};

const T_MAX: f64 = std::f64::consts::TAU;

fn build_points() -> (Vec<f64>, Vec<f64>) {
    let n = 300;
    let x: Vec<f64> = (0..n).map(|i| i as f64 / (n - 1) as f64 * T_MAX).collect();
    let y: Vec<f64> = x.iter().map(|&t| t.sin() * t.cos()).collect();
    (x, y)
}

struct LabelsApp {
    plot: Plot,
}

impl LabelsApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        let (x, y) = build_points();
        let curve = CurveData::new(x, y, Color32::from_rgb(255, 170, 90)).with_width(2.0);
        set_curve(render_state, 0, &curve);

        let mut plot = Plot::new(0);
        plot.limits = (0.0, T_MAX, -0.6, 0.6);
        plot.title = Some("sin(t)·cos(t)".to_owned());
        plot.x_label = Some("t  [rad]".to_owned());
        plot.y_label = Some("amplitude".to_owned());
        // Warm foreground for the frame/ticks/labels.
        plot.foreground = Some(Color32::from_rgb(230, 210, 180));

        Self { plot }
    }
}

impl eframe::App for LabelsApp {
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
        "rsplot · labels",
        options,
        Box::new(|cc| Ok(Box::new(LabelsApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
