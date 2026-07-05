//! High-level curve-ROI statistics example (silx `CurvesROIWidget`).
//!
//! Mirrors silx `plotCurveLegendWidget`/`CurvesROIWidget` usage: a 1D curve with
//! named `x`-range ROIs and a table of per-ROI raw/net counts and raw/net area
//! for the active curve. Drag a ROI's edges on the plot and its row updates live.
//!
//! Run with: `cargo run --example high_level_curves_roi`

use eframe::egui;
use rsplot::{CurvesRoiWidget, ManagedRoi, Plot1D, PlotWidget, Roi};

const N: usize = 400;

struct CurvesRoiApp {
    plot: Plot1D,
    roi_stats: CurvesRoiWidget,
}

impl CurvesRoiApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        let mut plot = Plot1D::new(render_state, 0);
        plot.set_graph_title("Curve ROI stats");
        plot.set_graph_cursor(true);

        let x = x_values();
        let y = peak_on_baseline(&x);
        let curve = plot.add_curve_with_legend(&x, &y, egui::Color32::YELLOW, "spectrum");
        plot.set_active_item(Some(curve));

        reset_rois(&mut plot);
        plot.drain_events();

        Self {
            plot,
            roi_stats: CurvesRoiWidget::new(),
        }
    }
}

impl eframe::App for CurvesRoiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::right("curves_roi_stats")
            .resizable(true)
            .default_size(420.0)
            .show_inside(ui, |ui| {
                ui.heading("Curve ROI stats");
                // One row per x-range ROI, reduced over the active curve (silx
                // CurvesROIWidget). Drag a ROI edge on the plot to update a row.
                self.plot.show_curves_roi_widget(ui, &mut self.roi_stats);
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.plot.show_toolbar_with(ui, |ui, plot| {
                if ui.button("Reset ROIs").clicked() {
                    reset_rois(plot);
                }
            });
            self.plot.show(ui);
        });
    }
}

fn reset_rois(plot: &mut PlotWidget) {
    plot.clear_rois();
    // Named x-range ROIs: one over the peak, one over the flat left baseline,
    // and one spanning the whole curve.
    for (name, lo, hi) in [
        ("peak", 4.5, 7.5),
        ("baseline", 0.0, 3.0),
        ("full", 0.0, 12.0),
    ] {
        let mut roi = ManagedRoi::new(Roi::VRange { x: (lo, hi) });
        roi.name = name.to_owned();
        plot.add_managed_roi(roi);
    }
}

fn x_values() -> Vec<f64> {
    (0..N).map(|i| i as f64 / (N - 1) as f64 * 12.0).collect()
}

/// A Gaussian peak at x = 6 over a gently rising linear baseline, so the net
/// counts/area (background-subtracted) differ visibly from the raw values.
fn peak_on_baseline(x: &[f64]) -> Vec<f64> {
    x.iter()
        .map(|&x| {
            let baseline = 1.0 + 0.3 * x;
            let peak = 8.0 * (-((x - 6.0).powi(2)) / 0.8).exp();
            baseline + peak
        })
        .collect()
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "rsplot - high-level curve ROI stats",
        options,
        Box::new(|cc| Ok(Box::new(CurvesRoiApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
