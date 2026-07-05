//! Mask Tools Example.
//!
//! Demonstrates the low-level [`MaskToolsWidget`] driving an interactive mask
//! over a bare [`Plot2D`]: pencil / eraser brushes and rectangle / polygon /
//! ellipse shape tools, each with a live cursor preview, shown as a colored
//! overlay (silx `MaskToolsWidget`). While a tool is active the plot is put in
//! [`PlotInteractionMode::MaskDraw`] so the primary drag paints the mask instead
//! of panning / zooming; with no tool selected the plot zooms normally.
//!
//! `MaskToolsWidget::handle_draw` drives the active tool and paints its preview;
//! `apply` uploads the resulting overlay to the plot.
//!
//! Run with: `cargo run --example high_level_mask_tools`

use eframe::egui;
use rsplot::{Colormap, MaskTool, MaskToolsWidget, Plot2D, PlotInteractionMode};

const WIDTH: u32 = 128;
const HEIGHT: u32 = 96;

struct MaskToolsApp {
    image_plot: Plot2D,
    mask_tools: MaskToolsWidget,
}

impl MaskToolsApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let pixels = build_image();

        let mut image_plot = Plot2D::new(rs, 0);
        image_plot.set_graph_title("Interactive Masking (Hover and Drag)");
        image_plot.set_graph_cursor(true);
        image_plot.set_default_colormap(Colormap::viridis(0.0, 1.0));
        image_plot
            .try_add_default_image(WIDTH, HEIGHT, &pixels)
            .expect("image dimensions match");

        let mut mask_tools = MaskToolsWidget::new(WIDTH, HEIGHT);
        // Start on the pencil so the demo is usable on launch (the toolbar
        // switches to the eraser / rectangle / polygon / ellipse tools).
        mask_tools.active_tool = MaskTool::Pencil;

        Self {
            image_plot,
            mask_tools,
        }
    }
}

impl eframe::App for MaskToolsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.vertical(|ui| {
            // Tool selector + level / color / brush / undo controls.
            self.mask_tools.show_toolbar(ui);

            ui.separator();

            // Reserve the primary drag for drawing while a tool is active (silx
            // mask-draw mode); otherwise leave the plot in its normal zoom mode.
            // Set before `show` so this frame's drag is already handled in the
            // right mode.
            let want = if self.mask_tools.active_tool != MaskTool::None {
                PlotInteractionMode::MaskDraw
            } else {
                PlotInteractionMode::Zoom
            };
            if self.image_plot.interaction_mode() != want {
                self.image_plot.set_interaction_mode(want);
            }

            let plot_resp = self.image_plot.show(ui);

            // Drive the active tool (pencil / eraser / rectangle / polygon /
            // ellipse) and paint its live cursor preview, then upload the
            // resulting overlay.
            self.mask_tools.handle_draw(ui, &plot_resp);
            self.mask_tools.apply(&mut self.image_plot);
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
        "rsplot: Mask Tools",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(MaskToolsApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
