//! Filled curves and stacked histograms example.
//!
//! Mirrors silx `examples/exampleBaseline.py`: demonstrates per-point baselines
//! for filled band areas (e.g. mean ± std) and stacked histograms where each
//! layer's baseline is the cumulative top of all previous layers.
//!
//! Run with: `cargo run --example high_level_baseline`

use eframe::egui;
use rsplot::{Baseline, CurveData, Plot1D};
use std::f64::consts::PI;

const N: usize = 200;

struct BaselineApp {
    band_plot: Plot1D,
    stack_plot: Plot1D,
}

impl BaselineApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        // --- Band plot: mean curve with ± std filled area ---
        let mut band_plot = Plot1D::new(rs, 0);
        band_plot.set_graph_title("Filled band (mean ± std)");

        let x: Vec<f64> = (0..N).map(|i| i as f64 * 2.0 * PI / N as f64).collect();
        let mean: Vec<f64> = x.iter().map(|&t| t.sin()).collect();
        let upper: Vec<f64> = mean.iter().map(|&m| m + 0.3).collect();
        let lower: Vec<f64> = mean.iter().map(|&m| m - 0.3).collect();

        // Filled upper band: curve = upper, baseline = mean
        let mut upper_data = CurveData::new(
            x.clone(),
            upper,
            egui::Color32::from_rgba_unmultiplied(100, 180, 255, 80),
        );
        upper_data.fill = true;
        upper_data.baseline = Baseline::PerPoint(mean.clone());
        band_plot.add_curve_data(&upper_data);

        // Filled lower band: curve = lower, baseline = mean
        let mut lower_data = CurveData::new(
            x.clone(),
            lower,
            egui::Color32::from_rgba_unmultiplied(100, 180, 255, 80),
        );
        lower_data.fill = true;
        lower_data.baseline = Baseline::PerPoint(mean.clone());
        band_plot.add_curve_data(&lower_data);

        // Mean curve on top
        band_plot.add_curve_with_legend(&x, &mean, egui::Color32::LIGHT_BLUE, "mean");

        // --- Stack plot: stacked histograms ---
        let mut stack_plot = Plot1D::new(rs, 1);
        stack_plot.set_graph_title("Stacked histograms");

        let edges: Vec<f64> = (0..=20).map(|i| i as f64 * 0.5).collect();
        let colors = [
            egui::Color32::from_rgba_unmultiplied(255, 80, 80, 200),
            egui::Color32::from_rgba_unmultiplied(80, 200, 80, 200),
            egui::Color32::from_rgba_unmultiplied(80, 80, 255, 200),
        ];
        let layers: Vec<Vec<f64>> = vec![
            (0..20)
                .map(|i| (i as f64 * 0.3).sin().abs() * 3.0 + 0.5)
                .collect(),
            (0..20)
                .map(|i| (i as f64 * 0.5 + 1.0).cos().abs() * 2.0 + 0.3)
                .collect(),
            (0..20)
                .map(|i| (i as f64 * 0.2 + 2.0).sin().abs() * 1.5 + 0.2)
                .collect(),
        ];

        let mut cumulative: Vec<f64> = vec![0.0; 20];
        for (layer, &color) in layers.iter().zip(colors.iter()) {
            let top: Vec<f64> = layer
                .iter()
                .zip(cumulative.iter())
                .map(|(v, c)| v + c)
                .collect();
            // Build step histogram x/y from edges+counts.
            let (hx, hy) = histogram_step_xy(&edges, &top);
            let (_, hbase) = histogram_step_xy(&edges, &cumulative);

            let mut data = CurveData::new(hx, hy, color);
            data.fill = true;
            data.baseline = Baseline::PerPoint(hbase);
            stack_plot.add_curve_data(&data);

            cumulative = top;
        }

        Self {
            band_plot,
            stack_plot,
        }
    }
}

impl eframe::App for BaselineApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let half = ui.available_size() * egui::vec2(1.0, 0.5);
        ui.allocate_ui(half, |ui| {
            self.band_plot.show(ui);
        });
        ui.allocate_ui(half, |ui| {
            self.stack_plot.show(ui);
        });
    }
}

/// Convert histogram `edges` (n+1) + `counts` (n) to step-curve x/y pairs.
fn histogram_step_xy(edges: &[f64], counts: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let n = counts.len();
    let mut x = Vec::with_capacity(2 * n);
    let mut y = Vec::with_capacity(2 * n);
    for i in 0..n {
        x.push(edges[i]);
        x.push(edges[i + 1]);
        y.push(counts[i]);
        y.push(counts[i]);
    }
    (x, y)
}

fn main() -> eframe::Result {
    eframe::run_native(
        "rsplot: baseline curves and stacked histograms",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(BaselineApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
