//! ImageView example.
//!
//! Mirrors silx `examples/imageview.py`: a central image plot flanked by
//! column-sum (horizontal) and row-sum (vertical) profile histograms.
//! The histogram axes track the image limits via SyncAxes.
//!
//! Run with: `cargo run --example high_level_image_view`

use eframe::egui;
use egui_silx::{Colormap, ImageView};

const WIDTH: u32 = 128;
const HEIGHT: u32 = 96;

struct ImageViewApp {
    view: ImageView,
}

impl ImageViewApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer");

        let pixels = build_image();
        let mut view = ImageView::new(rs, 0);
        view.set_image(WIDTH, HEIGHT, &pixels, Colormap::viridis(0.0, 1.0))
            .expect("image dimensions match");
        view.image_plot_mut().set_graph_title("ImageView");

        Self { view }
    }
}

impl eframe::App for ImageViewApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.view.show(ui, None, None);
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
        "egui-silx: image view",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(ImageViewApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
