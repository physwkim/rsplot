//! Limits Widget Example.
//!
//! Demonstrates the `LimitsWidget` for manually configuring plot axes and limits.
//!
//! Run with: `cargo run --example high_level_limits_widget`

use eframe::egui;
use rsplot::{Colormap, LimitsWidget, Plot2D};

const WIDTH: u32 = 128;
const HEIGHT: u32 = 96;

struct LimitsWidgetApp {
    image_plot: Plot2D,
    limits_widget: LimitsWidget,
}

impl LimitsWidgetApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let pixels = build_image();

        let mut image_plot = Plot2D::new(rs, 0);
        image_plot.set_graph_title("Interactive Limits");
        image_plot.set_default_colormap(Colormap::viridis(0.0, 1.0));
        image_plot
            .try_add_default_image(WIDTH, HEIGHT, &pixels)
            .expect("image dimensions match");

        let mut limits_widget = LimitsWidget::new();
        limits_widget.open = true; // Show by default

        Self {
            image_plot,
            limits_widget,
        }
    }
}

impl eframe::App for LimitsWidgetApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.vertical(|ui| {
            if ui.button("Toggle Limits Widget").clicked() {
                self.limits_widget.open = !self.limits_widget.open;
            }

            ui.separator();

            self.image_plot.show_with_toolbar(ui);

            // Show the floating window and allow it to manage limits
            self.limits_widget.show(ui.ctx(), &mut self.image_plot);
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
        "rsplot: Limits Widget",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(LimitsWidgetApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
