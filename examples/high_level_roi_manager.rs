//! ROI Manager Example.
//!
//! Demonstrates the `RoiManagerWidget` for tracking multiple Regions of Interest on a plot.
//!
//! Run with: `cargo run --example high_level_roi_manager`

use eframe::egui;
use egui_silx::{Colormap, Plot2D, RoiManagerWidget};

const WIDTH: u32 = 128;
const HEIGHT: u32 = 96;

struct RoiManagerApp {
    image_plot: Plot2D,
    roi_manager: RoiManagerWidget,
}

impl RoiManagerApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let pixels = build_image();

        let mut image_plot = Plot2D::new(rs, 0);
        image_plot.set_graph_title("Interactive ROI Manager");
        image_plot.set_default_colormap(Colormap::viridis(0.0, 1.0));
        image_plot
            .try_add_default_image(WIDTH, HEIGHT, &pixels)
            .expect("image dimensions match");

        let mut roi_manager = RoiManagerWidget::new();
        roi_manager.open = true; // Show by default

        Self {
            image_plot,
            roi_manager,
        }
    }
}

impl eframe::App for RoiManagerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.vertical(|ui| {
            if ui.button("Toggle ROI Manager").clicked() {
                self.roi_manager.open = !self.roi_manager.open;
            }

            ui.separator();

            self.image_plot.show_with_toolbar(ui);

            // Show the floating window to manage ROIs
            self.roi_manager.show(ui.ctx(), &mut self.image_plot);
        });
    }
}

fn build_image() -> Vec<f32> {
    let mut pixels = Vec::with_capacity((WIDTH * HEIGHT) as usize);
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let cx = (col as f32 - WIDTH as f32 / 2.0) / (WIDTH as f32 / 4.0);
            let cy = (row as f32 - HEIGHT as f32 / 2.0) / (HEIGHT as f32 / 4.0);
            pixels.push((-0.5 * (cx * cx + cy * cy)).exp());
        }
    }
    pixels
}

fn main() -> eframe::Result {
    eframe::run_native(
        "egui-silx: ROI Manager",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(RoiManagerApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
