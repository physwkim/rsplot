//! Colormap Dialog Example.
//!
//! Demonstrates the `ColormapDialog` for interactively editing a colormap applied to a `Plot2D`.
//!
//! Run with: `cargo run --example high_level_colormap_dialog`

use eframe::egui;
use rsplot::{Colormap, ColormapDialog, Plot2D};

const WIDTH: u32 = 128;
const HEIGHT: u32 = 96;

struct ColormapDialogApp {
    image_plot: Plot2D,
    colormap_dialog: ColormapDialog,
}

impl ColormapDialogApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let pixels = build_image();

        let mut image_plot = Plot2D::new(rs, 0);
        image_plot.set_graph_title("Interactive Colormap Editing");
        let initial_cmap = Colormap::viridis(0.0, 1.0);
        image_plot.set_default_colormap(initial_cmap.clone());
        image_plot
            .try_add_default_image(WIDTH, HEIGHT, &pixels)
            .expect("image dimensions match");

        let mut colormap_dialog = ColormapDialog::new().with_colormap(&initial_cmap);
        colormap_dialog.open = true; // Show by default

        Self {
            image_plot,
            colormap_dialog,
        }
    }
}

impl eframe::App for ColormapDialogApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.vertical(|ui| {
            // The colormap toggle sits in the plot toolbar row as a checkable
            // action, mirroring silx ColormapAction.
            let dialog = &mut self.colormap_dialog;
            self.image_plot.show_toolbar_with(ui, |ui, _plot| {
                ui.separator();
                dialog.toggle_button(ui);
            });
            self.image_plot.show(ui);

            // Show the colormap dialog floating window, which updates the plot colormap internally
            self.colormap_dialog.show(ui.ctx(), &mut self.image_plot);
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
        "rsplot: Colormap Dialog",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(ColormapDialogApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
