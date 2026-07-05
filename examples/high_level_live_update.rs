//! High-level live-update example.
//!
//! Mirrors silx `plotUpdateCurveFromThread.py` at the egui level: updates are
//! applied on the UI thread each frame, reusing the same curve handle.
//!
//! Run with: `cargo run --example high_level_live_update`

use eframe::egui;
use rsplot::{CurveSpec, GraphGrid, Plot1D};

const N: usize = 1_000;

struct LiveUpdateApp {
    plot: Plot1D,
    curve: rsplot::ItemHandle,
    x: Vec<f64>,
    paused: bool,
    amplitude: f64,
}

impl LiveUpdateApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        let mut plot = Plot1D::new(render_state, 0);
        plot.set_graph_title("Live curve update");
        plot.set_graph_grid_mode(GraphGrid::MajorAndMinor);
        plot.set_graph_cursor(true);

        let x = x_values();
        let y = curve_values(&x, 0.0, 1.0);
        let curve = plot.add_curve_with_legend(&x, &y, egui::Color32::LIGHT_GREEN, "stream");
        plot.set_limits(0.0, 20.0, -1.5, 1.5, None);
        plot.set_auto_reset_zoom(false);
        plot.drain_events();

        Self {
            plot,
            curve,
            x,
            paused: false,
            amplitude: 1.0,
        }
    }

    fn update_curve(&mut self, phase: f64) {
        let y = curve_values(&self.x, phase, self.amplitude);
        self.plot.update_curve_spec(
            self.curve,
            CurveSpec::new(&self.x, &y, egui::Color32::LIGHT_GREEN),
        );
    }
}

impl eframe::App for LiveUpdateApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::right("live_update_stats")
            .resizable(true)
            .default_size(180.0)
            .show_inside(ui, |ui| {
                ui.heading("Stats");
                self.plot.show_active_stats(ui);
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            let mut paused = self.paused;
            let mut amplitude = self.amplitude;
            self.plot.show_toolbar_with(ui, |ui, _plot| {
                ui.checkbox(&mut paused, "Paused");
                ui.add(
                    egui::Slider::new(&mut amplitude, 0.1..=1.4)
                        .text("amplitude")
                        .max_decimals(2),
                );
            });
            self.paused = paused;
            self.amplitude = amplitude;
            if !self.paused {
                let time = ui.input(|input| input.time);
                self.update_curve(time * 3.0);
                ui.ctx().request_repaint();
            }
            self.plot.show(ui);
        });
    }
}

fn x_values() -> Vec<f64> {
    (0..N).map(|i| i as f64 / (N - 1) as f64 * 20.0).collect()
}

fn curve_values(x: &[f64], phase: f64, amplitude: f64) -> Vec<f64> {
    x.iter()
        .map(|x| amplitude * (x * 2.0 + phase).sin() + 0.15 * (x * 9.0 - phase).cos())
        .collect()
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "rsplot - high-level live update",
        options,
        Box::new(|cc| Ok(Box::new(LiveUpdateApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
