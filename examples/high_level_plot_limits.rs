//! Axis range-constraint example.
//!
//! Mirrors silx `plotLimits.py`: sliders set per-axis min/max range and
//! min/max position constraints. Pan and zoom are live so the effect is
//! immediately visible.
//!
//! Run with: `cargo run --example high_level_plot_limits`

use eframe::egui;
use rsplot::Plot1D;

struct PlotLimitsApp {
    plot: Plot1D,
    x_min_range: f64,
    x_max_range: f64,
    y_min_range: f64,
    y_max_range: f64,
    x_min_pos: f64,
    x_max_pos: f64,
    y_min_pos: f64,
    y_max_pos: f64,
    constrain_range: bool,
    constrain_pos: bool,
}

impl PlotLimitsApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        let mut plot = Plot1D::new(render_state, 0);
        plot.set_graph_title("Axis range constraints");

        let x: Vec<f64> = (0..=100).map(|i| i as f64 * 0.1).collect();
        let y: Vec<f64> = x.iter().map(|&t| (t * 2.0).sin()).collect();
        plot.add_curve_with_legend(&x, &y, egui::Color32::YELLOW, "sin");

        let y2: Vec<f64> = x.iter().map(|&t| (t * 3.0).cos() * 0.5).collect();
        plot.add_curve_with_legend(&x, &y2, egui::Color32::LIGHT_BLUE, "cos");

        Self {
            plot,
            x_min_range: 1.0,
            x_max_range: 10.0,
            y_min_range: 0.5,
            y_max_range: 5.0,
            x_min_pos: -1.0,
            x_max_pos: 11.0,
            y_min_pos: -2.0,
            y_max_pos: 2.0,
            constrain_range: false,
            constrain_pos: false,
        }
    }

    fn apply_constraints(&mut self) {
        if self.constrain_range {
            self.plot.set_x_min_range(Some(self.x_min_range));
            self.plot.set_x_max_range(Some(self.x_max_range));
            self.plot.set_y_min_range(Some(self.y_min_range));
            self.plot.set_y_max_range(Some(self.y_max_range));
        } else {
            self.plot.set_x_min_range(None);
            self.plot.set_x_max_range(None);
            self.plot.set_y_min_range(None);
            self.plot.set_y_max_range(None);
        }
        if self.constrain_pos {
            self.plot.set_x_min_pos(Some(self.x_min_pos));
            self.plot.set_x_max_pos(Some(self.x_max_pos));
            self.plot.set_y_min_pos(Some(self.y_min_pos));
            self.plot.set_y_max_pos(Some(self.y_max_pos));
        } else {
            self.plot.set_x_min_pos(None);
            self.plot.set_x_max_pos(None);
            self.plot.set_y_min_pos(None);
            self.plot.set_y_max_pos(None);
        }
    }
}

impl eframe::App for PlotLimitsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::left("controls")
            .resizable(true)
            .default_size(200.0)
            .show_inside(ui, |ui| {
                ui.heading("Range constraints");
                ui.checkbox(&mut self.constrain_range, "Enable range limits");
                ui.add_enabled(
                    self.constrain_range,
                    egui::Slider::new(&mut self.x_min_range, 0.1..=5.0).text("X min span"),
                );
                ui.add_enabled(
                    self.constrain_range,
                    egui::Slider::new(&mut self.x_max_range, 1.0..=20.0).text("X max span"),
                );
                ui.add_enabled(
                    self.constrain_range,
                    egui::Slider::new(&mut self.y_min_range, 0.1..=2.0).text("Y min span"),
                );
                ui.add_enabled(
                    self.constrain_range,
                    egui::Slider::new(&mut self.y_max_range, 0.5..=10.0).text("Y max span"),
                );

                ui.separator();
                ui.heading("Position constraints");
                ui.checkbox(&mut self.constrain_pos, "Enable position limits");
                ui.add_enabled(
                    self.constrain_pos,
                    egui::Slider::new(&mut self.x_min_pos, -5.0..=0.0).text("X min pos"),
                );
                ui.add_enabled(
                    self.constrain_pos,
                    egui::Slider::new(&mut self.x_max_pos, 10.0..=20.0).text("X max pos"),
                );
                ui.add_enabled(
                    self.constrain_pos,
                    egui::Slider::new(&mut self.y_min_pos, -5.0..=0.0).text("Y min pos"),
                );
                ui.add_enabled(
                    self.constrain_pos,
                    egui::Slider::new(&mut self.y_max_pos, 0.0..=5.0).text("Y max pos"),
                );

                ui.separator();
                let cx = self.plot.x_constraints();
                ui.label(format!("X range [{:?}, {:?}]", cx.min_range, cx.max_range));
                ui.label(format!("X pos [{:?}, {:?}]", cx.min_pos, cx.max_pos));
                let cy = self.plot.y_constraints();
                ui.label(format!("Y range [{:?}, {:?}]", cy.min_range, cy.max_range));
                ui.label(format!("Y pos [{:?}, {:?}]", cy.min_pos, cy.max_pos));
            });

        self.apply_constraints();

        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.plot.show_toolbar(ui);
            self.plot.show(ui);
        });
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "rsplot: plot limits",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(PlotLimitsApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
