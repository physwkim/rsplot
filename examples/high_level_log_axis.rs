//! Logarithmic-axis example.
//!
//! Mirrors silx `examples/pygfx_backend/07_log_axes.py`: two panels —
//! one with a log Y axis (power-law rendered as a straight line) and one
//! with log X axis (exponential decay stretched to linear-looking).
//!
//! High-level API: `PlotWidget::set_y_log(true)` / `set_x_log(true)`.
//! The toolbar log-scale toggle buttons also call these methods at runtime.
//!
//! Run with: `cargo run --example high_level_log_axis`

use eframe::egui;
use rsplot::{Plot1D, YAxis};

const N: usize = 200;

struct LogAxisApp {
    log_y: Plot1D,
    log_x: Plot1D,
}

impl LogAxisApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        // --- Panel A: power law on a log Y axis ---
        // y = 10^(x/12)  spans four decades (1..1e4) as x ∈ [0, 48].
        // On a linear scale this is an explosive exponential; on log10 it is
        // a straight line.
        let mut log_y = Plot1D::new(rs, 0);
        log_y.set_graph_title("Log Y — power law y = 10^(x/12)");
        log_y.set_graph_x_label("x");
        log_y.set_graph_y_label("y  (log scale)", YAxis::Left);

        let x_a: Vec<f64> = (0..N).map(|i| 48.0 * i as f64 / (N - 1) as f64).collect();
        let y_a: Vec<f64> = x_a.iter().map(|&x| 10f64.powf(x / 12.0)).collect();
        log_y.add_curve_with_legend(
            &x_a,
            &y_a,
            egui::Color32::from_rgb(120, 200, 255),
            "10^(x/12)",
        );

        // Limits must be strictly positive for a log axis.
        log_y.set_graph_x_limits(-1.0, 49.0);
        log_y.set_graph_y_limits(0.5, 2e4, YAxis::Left);
        log_y.set_y_log(true);

        // --- Panel B: exponential decay on a log X axis ---
        // y = exp(-x)  over x ∈ [0.1, 100].  Log X maps the three-decade
        // x span to equal screen width; the decay is nearly linear on log X.
        let mut log_x = Plot1D::new(rs, 1);
        log_x.set_graph_title("Log X — exponential decay y = exp(-x)");
        log_x.set_graph_x_label("x  (log scale)");
        log_x.set_graph_y_label("y", YAxis::Left);

        let x_b: Vec<f64> = (0..N)
            .map(|i| 10f64.powf(-1.0 + 3.0 * i as f64 / (N - 1) as f64))
            .collect();
        let y_b: Vec<f64> = x_b.iter().map(|&x| (-x).exp()).collect();
        log_x.add_curve_with_legend(&x_b, &y_b, egui::Color32::from_rgb(255, 160, 90), "exp(-x)");

        log_x.set_graph_x_limits(0.05, 200.0);
        log_x.set_x_log(true);

        Self { log_y, log_x }
    }
}

impl eframe::App for LogAxisApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let half = ui.available_size() * egui::vec2(0.5, 1.0);
        ui.horizontal(|ui| {
            ui.allocate_ui(half, |ui| {
                self.log_y.show(ui);
            });
            ui.allocate_ui(half, |ui| {
                self.log_x.show(ui);
            });
        });
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "rsplot: log axes",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(LogAxisApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
