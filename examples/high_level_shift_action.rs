//! High-level custom plot action example.
//!
//! Mirrors silx `shiftPlotAction.py`: a custom action shifts the active curve
//! by one unit while retaining its item handle.
//!
//! Run with: `cargo run --example high_level_shift_action`

use eframe::egui;
use egui_silx::{CurveSpec, GraphGrid, ItemHandle, Plot1D};

struct CurveEntry {
    handle: ItemHandle,
    legend: &'static str,
    x: Vec<f64>,
    y: Vec<f64>,
    color: egui::Color32,
}

struct ShiftActionApp {
    plot: Plot1D,
    curves: Vec<CurveEntry>,
    status: String,
}

impl ShiftActionApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        let mut plot = Plot1D::new(render_state, 0);
        plot.set_graph_title("Shift active curve");
        plot.set_graph_grid_mode(GraphGrid::MajorAndMinor);

        let x: Vec<f64> = (0..=6).map(f64::from).collect();
        let triangle = vec![0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0];
        let oblique = vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0];

        let triangle_handle = plot.add_curve_with_legend(
            &x,
            &triangle,
            egui::Color32::LIGHT_GREEN,
            "triangle shaped curve",
        );
        let oblique_handle =
            plot.add_curve_with_legend(&x, &oblique, egui::Color32::LIGHT_BLUE, "oblique line");
        plot.set_active_curve(Some(triangle_handle));
        plot.drain_events();

        Self {
            plot,
            curves: vec![
                CurveEntry {
                    handle: triangle_handle,
                    legend: "triangle shaped curve",
                    x: x.clone(),
                    y: triangle,
                    color: egui::Color32::LIGHT_GREEN,
                },
                CurveEntry {
                    handle: oblique_handle,
                    legend: "oblique line",
                    x,
                    y: oblique,
                    color: egui::Color32::LIGHT_BLUE,
                },
            ],
            status: "select a curve, then shift it".to_owned(),
        }
    }

    fn shift_active_curve_up(&mut self) {
        let Some(handle) = self.plot.active_curve() else {
            self.status = "no active curve selected".to_owned();
            return;
        };

        let Some(curve) = self.curves.iter_mut().find(|curve| curve.handle == handle) else {
            self.status = "active item is not one of the retained curves".to_owned();
            return;
        };

        for y in &mut curve.y {
            *y += 1.0;
        }
        self.plot
            .update_curve_spec(handle, CurveSpec::new(&curve.x, &curve.y, curve.color));
        self.status = format!("shifted {}", curve.legend);
    }
}

impl eframe::App for ShiftActionApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::right("shift_action_legend")
            .resizable(true)
            .default_size(220.0)
            .show_inside(ui, |ui| {
                ui.heading("Legends");
                self.plot.show_legend(ui);
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            let status = self.status.clone();
            let (_, shift_clicked) = self.plot.show_toolbar_with(ui, |ui, _plot| {
                let shift_clicked = ui.button("Shift active curve up").clicked();
                ui.label(status);
                shift_clicked
            });
            if shift_clicked {
                self.shift_active_curve_up();
            }
            self.plot.show(ui);
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "egui-silx - high-level shift action",
        options,
        Box::new(|cc| Ok(Box::new(ShiftActionApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
