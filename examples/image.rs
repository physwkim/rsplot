//! Image example: render a 2D scalar field as a viridis-colormapped image.
//!
//! The field is a diagonal ramp (low at bottom-left, high at top-right) plus a
//! Gaussian bump, so orientation and the colormap are both visually checkable:
//! bottom-left should be dark purple, top-right bright yellow, with a brighter
//! blob in the upper-left region.
//!
//! Run with: `cargo run --example image`

use eframe::egui;
use rsplot::{Colormap, ImageData, Plot, PlotView, install, set_image};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 192;

fn build_image() -> ImageData {
    let mut data = vec![0.0f32; (WIDTH * HEIGHT) as usize];
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let u = col as f32 / (WIDTH - 1) as f32; // 0..1 left→right
            let v = row as f32 / (HEIGHT - 1) as f32; // 0..1 bottom→top
            let ramp = u + v; // 0..2 diagonal
            let (du, dv) = (u - 0.3, v - 0.7); // bump near upper-left
            let bump = 1.5 * (-(du * du + dv * dv) / 0.01).exp();
            data[(row * WIDTH + col) as usize] = ramp + bump;
        }
    }
    ImageData::new(WIDTH, HEIGHT, data, Colormap::viridis(0.0, 2.5))
}

struct ImageApp {
    plot: Plot,
}

impl ImageApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        let image = build_image();
        set_image(render_state, 0, &image);

        let mut plot = Plot::new(0);
        // Limits == image extent so the image fills the data area.
        plot.limits = (0.0, image.width as f64, 0.0, image.height as f64);
        // Mirror the image colormap so the colorbar shows the same LUT/clim.
        plot.colormap = image.colormap().cloned();

        Self { plot }
    }
}

impl eframe::App for ImageApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            PlotView::new().show(ui, &mut self.plot);
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "rsplot · image",
        options,
        Box::new(|cc| Ok(Box::new(ImageApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
