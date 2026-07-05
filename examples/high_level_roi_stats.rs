//! High-level ROI statistics example.
//!
//! Mirrors the 2D-image part of silx `plotROIStats.py`: a Plot2D image with
//! draggable ROIs and statistics computed from the selected image pixels.
//!
//! Run with: `cargo run --example high_level_roi_stats`

use eframe::egui;
use rsplot::{Colormap, Plot2D, PlotWidget, Roi, RoiStatsWidget};

const WIDTH: u32 = 180;
const HEIGHT: u32 = 140;

struct RoiStatsApp {
    plot: Plot2D,
    roi_stats: RoiStatsWidget,
}

impl RoiStatsApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        let mut plot = Plot2D::new(render_state, 0);
        plot.set_graph_title("ROI stats");
        plot.set_graph_cursor(true);
        plot.set_default_colormap(Colormap::viridis(-0.4, 1.2));

        let image = build_image();
        let image_handle = plot
            .try_add_default_image(WIDTH, HEIGHT, &image)
            .expect("generated image length matches dimensions");
        plot.set_item_legend(image_handle, "image");
        plot.add_roi(Roi::Rect {
            x: (30.0, 100.0),
            y: (25.0, 90.0),
        });
        plot.add_roi(Roi::HRange { y: (55.0, 80.0) });
        plot.add_roi(Roi::VRange { x: (115.0, 145.0) });
        plot.drain_events();

        Self {
            plot,
            roi_stats: RoiStatsWidget::new(),
        }
    }
}

impl eframe::App for RoiStatsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::right("roi_stats")
            .resizable(true)
            .default_size(320.0)
            .show_inside(ui, |ui| {
                ui.heading("ROI stats");
                // One row per ROI, reduced over the active image's pixels inside
                // each ROI (silx ROIStatsWidget). The table follows the active
                // item and the live ROI list.
                self.plot.show_roi_stats_widget(ui, &mut self.roi_stats);
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
    plot.add_roi(Roi::Rect {
        x: (30.0, 100.0),
        y: (25.0, 90.0),
    });
    plot.add_roi(Roi::HRange { y: (55.0, 80.0) });
    plot.add_roi(Roi::VRange { x: (115.0, 145.0) });
}

fn build_image() -> Vec<f32> {
    let mut data = vec![0.0; (WIDTH * HEIGHT) as usize];
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let x = -4.0 + 8.0 * col as f32 / (WIDTH - 1) as f32;
            let y = -3.0 + 6.0 * row as f32 / (HEIGHT - 1) as f32;
            let wave = (x * 2.0).sin() * (y * 1.5).cos();
            let spot = (-((x - 1.1).powi(2) + (y + 0.6).powi(2)) / 0.45).exp();
            data[(row * WIDTH + col) as usize] = 0.45 * wave + spot;
        }
    }
    data
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "rsplot - high-level ROI stats",
        options,
        Box::new(|cc| Ok(Box::new(RoiStatsApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
