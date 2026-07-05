//! ROI Manager Example.
//!
//! Demonstrates the `RoiManagerWidget` editing the plot's single ROI
//! collection: ROIs added or restyled in the manager window (color, name,
//! current/highlight, line width/style, fill) render on the plot immediately,
//! and in Select mode you can drag/resize them directly on the plot. The plot
//! starts with two styled, named ROIs; the circle is the current (highlighted)
//! ROI.
//!
//! Run with: `cargo run --example high_level_roi_manager`

use eframe::egui;
use egui::Color32;
use rsplot::{Colormap, Plot2D, PlotInteractionMode, Roi, RoiManagerWidget};

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

        // Select mode lets you drag/resize the ROIs directly on the plot.
        image_plot.set_interaction_mode(PlotInteractionMode::Select);

        // Seed two styled, named ROIs. They live on the plot (one collection)
        // and the manager window edits these same ROIs.
        let rect = image_plot.add_roi(Roi::Rect {
            x: (18.0, 58.0),
            y: (20.0, 52.0),
        });
        image_plot.set_roi_name(rect, "feature A");
        image_plot.set_roi_color(rect, Color32::from_rgb(90, 200, 255));

        let spot = image_plot.add_roi(Roi::Circle {
            center: (92.0, 60.0),
            radius: 16.0,
        });
        image_plot.set_roi_name(spot, "spot");
        image_plot.set_roi_color(spot, Color32::from_rgb(255, 180, 80));
        image_plot.set_roi_fill(spot, true);

        // Highlight the circle as the current ROI (thicker outline on the plot).
        image_plot.set_current_roi(Some(spot));
        image_plot.drain_events();

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
        "rsplot: ROI Manager",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(RoiManagerApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
