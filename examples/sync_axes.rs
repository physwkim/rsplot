//! Synchronized axes example.
//!
//! Mirrors silx `syncaxis.py` / `syncPlotLocation.py`: two independent plots
//! share the same X axis so panning or zooming one also updates the other.
//!
//! Run with: `cargo run --example sync_axes`

use eframe::egui;
use egui_silx::CurveData;
use egui_silx::{Plot, PlotView, SyncAxes, install, set_curve};

struct SyncAxesApp {
    plot_a: Plot,
    plot_b: Plot,
    sync: SyncAxes,
}

impl SyncAxesApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer")
            .clone();
        install(&render_state);

        let x: Vec<f64> = (0..=200).map(|i| i as f64 * 0.05).collect();

        // Plot A: sin curve
        let y_a: Vec<f64> = x.iter().map(|&t| t.sin()).collect();
        set_curve(
            &render_state,
            0,
            &CurveData::new(x.clone(), y_a, egui::Color32::YELLOW),
        );

        // Plot B: cos curve
        let y_b: Vec<f64> = x.iter().map(|&t| t.cos()).collect();
        set_curve(
            &render_state,
            1,
            &CurveData::new(x.clone(), y_b, egui::Color32::LIGHT_BLUE),
        );

        let mut plot_a = Plot::new(0);
        plot_a.limits = (0.0, 10.0, -1.2, 1.2);
        plot_a.title = Some("Plot A (sin)".into());

        let mut plot_b = Plot::new(1);
        plot_b.limits = (0.0, 10.0, -1.2, 1.2);
        plot_b.title = Some("Plot B (cos)".into());

        Self {
            plot_a,
            plot_b,
            // Sync X only — the Y limits are independent.
            sync: SyncAxes::new().with_sync_y(false),
        }
    }
}

impl eframe::App for SyncAxesApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Synchronize X axis before rendering so both plots reflect the same
        // pan/zoom even on the first frame after user interaction.
        self.sync.sync(&mut [&mut self.plot_a, &mut self.plot_b]);

        let half = ui.available_size() * egui::vec2(1.0, 0.5);
        ui.vertical(|ui| {
            ui.allocate_ui(half, |ui| {
                PlotView::new().show(ui, &mut self.plot_a);
            });
            ui.allocate_ui(half, |ui| {
                PlotView::new().show(ui, &mut self.plot_b);
            });
        });
    }
}

fn main() -> eframe::Result {
    eframe::run_native(
        "egui-silx: synchronized axes",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(SyncAxesApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
