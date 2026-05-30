//! High-level clear-action example.
//!
//! Mirrors silx `plotClearAction.py`: a custom UI action clears a plot, and a
//! second action repopulates it with curve, scatter, and histogram items.
//!
//! Run with: `cargo run --example high_level_clear_action`

use eframe::egui;
use egui_silx::{GraphGrid, Plot1D, PlotWidget};

struct ClearActionApp {
    plot: Plot1D,
}

impl ClearActionApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        let mut plot = Plot1D::new(render_state, 0);
        plot.set_graph_title("Clear action");
        plot.set_graph_grid_mode(GraphGrid::MajorAndMinor);
        populate(&mut plot);
        plot.drain_events();
        Self { plot }
    }
}

impl eframe::App for ClearActionApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::right("clear_action_legend")
            .resizable(true)
            .default_size(220.0)
            .show_inside(ui, |ui| {
                ui.heading("Legends");
                self.plot.show_legend(ui);
                ui.separator();
                self.plot.show_active_stats(ui);
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.plot.show_toolbar_with(ui, |ui, plot| {
                if ui.button("Clear").clicked() {
                    plot.clear();
                }
                if ui.button("Repopulate").clicked() {
                    plot.clear();
                    populate(plot);
                }
                ui.label(format!("items: {}", plot.get_items().len()));
            });
            self.plot.show(ui);
        });
    }
}

fn populate(plot: &mut PlotWidget) {
    let x: Vec<f64> = (0..240).map(|i| i as f64 / 20.0).collect();
    let y: Vec<f64> = x.iter().map(|x| (x * 1.5).sin()).collect();
    let curve = plot.add_curve_with_legend(&x, &y, egui::Color32::YELLOW, "curve");

    let sx: Vec<f64> = (0..32).map(|i| i as f64 / 31.0 * 12.0).collect();
    let sy: Vec<f64> = sx.iter().map(|x| 0.7 * (x * 1.8).cos()).collect();
    plot.add_scatter_with_legend(&sx, &sy, egui::Color32::LIGHT_BLUE, "scatter");

    let edges: Vec<f64> = (0..=16).map(|i| i as f64 * 12.0 / 16.0).collect();
    let counts: Vec<f64> = edges
        .windows(2)
        .map(|edge| {
            let c = 0.5 * (edge[0] + edge[1]);
            0.4 + 1.8 * (-(c - 7.0).powi(2) / 8.0).exp()
        })
        .collect();
    plot.add_histogram_with_legend(&edges, &counts, egui::Color32::LIGHT_GREEN, "histogram")
        .expect("histogram edges are bins + 1");
    plot.set_active_item(Some(curve));
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "egui-silx - high-level clear action",
        options,
        Box::new(|cc| Ok(Box::new(ClearActionApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
