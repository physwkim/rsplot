//! Dual Y-axis (y2) example.
//!
//! Mirrors silx `examples/pygfx_backend/10_dual_yaxis.py`: two curves that
//! live on different Y scales share a common X axis. The blue damped oscillation
//! reads from the left axis (0..100), while the orange saturation ramp reads
//! from the right Y2 axis (0..1).
//!
//! The high-level API: use `CurveData::with_y_axis(YAxis::Right)` to bind a
//! curve to the secondary axis and `set_graph_y_limits(lo, hi, YAxis::Right)`
//! to set its range.
//!
//! Run with: `cargo run --example high_level_y2_axis`

use eframe::egui;
use egui_silx::{CurveData, Plot1D, YAxis};

const N: usize = 400;

struct Y2App {
    plot: Plot1D,
}

impl Y2App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let mut plot = Plot1D::new(rs, 0);
        plot.set_graph_title("Dual Y-axis");
        plot.set_graph_x_label("time (s)");
        plot.set_graph_y_label("amplitude", YAxis::Left);
        plot.set_graph_y_label("normalized", YAxis::Right);

        let xs: Vec<f64> = (0..N).map(|i| 10.0 * i as f64 / (N - 1) as f64).collect();

        // Left axis: damped oscillation (range 0..100).
        let left_y: Vec<f64> = xs
            .iter()
            .map(|&t| 50.0 + 40.0 * (t * 1.5).sin() * (-t / 8.0).exp())
            .collect();
        let left = CurveData::new(xs.clone(), left_y, egui::Color32::from_rgb(120, 180, 255))
            .with_width(2.0);
        let h = plot.add_curve_data(&left);
        plot.set_item_legend(h, "damped oscillation (left)");

        // Right Y2 axis: slow saturation ramp (range 0..1).
        let right_y: Vec<f64> = xs.iter().map(|&t| 1.0 - (-t / 3.0).exp()).collect();
        let right = CurveData::new(xs, right_y, egui::Color32::from_rgb(255, 160, 80))
            .with_width(2.0)
            .with_y_axis(YAxis::Right);
        let h2 = plot.add_curve_data(&right);
        plot.set_item_legend(h2, "saturation (right)");

        // Set explicit limits so each axis spans its natural range.
        plot.set_graph_y_limits(0.0, 100.0, YAxis::Left);
        plot.set_graph_y_limits(0.0, 1.0, YAxis::Right);

        Self { plot }
    }
}

impl eframe::App for Y2App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.plot.show(ui);
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "egui-silx: dual Y-axis",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(Y2App::new(cc)) as Box<dyn eframe::App>)),
    )
}
