//! Large-image example: an image wider than the device's single-texture limit,
//! forcing the tiling path (`doc/design.md` §13 D2).
//!
//! The width is set to `max_texture_dimension_2d + 64`, so the image is always
//! split into at least two column-tiles regardless of the GPU. The field is a
//! smooth horizontal gradient with thin vertical guide stripes every 256 pixels;
//! if the tiles were misaligned, the gradient would show a step and the stripes
//! would bend at a tile seam. A continuous gradient and straight stripes mean
//! the tiles abut exactly.
//!
//! Run with: `cargo run --release --example large_image`

use eframe::egui;
use egui_silx::{Colormap, ImageData, Plot, PlotWidget, install, set_image};

const HEIGHT: u32 = 256;

fn build_image(width: u32) -> ImageData {
    let mut data = vec![0.0f32; (width as usize) * (HEIGHT as usize)];
    for row in 0..HEIGHT {
        for col in 0..width {
            let u = col as f32 / (width - 1) as f32; // 0..1 left→right
            // Smooth gradient that a seam misalignment would visibly step.
            let mut v = u;
            // Thin guide stripes every 256 px: a seam would bend them.
            if col % 256 == 0 {
                v = 1.0;
            }
            data[(row * width + col) as usize] = v;
        }
    }
    ImageData::new(width, HEIGHT, data, Colormap::viridis(0.0, 1.0))
}

struct LargeImageApp {
    plot: Plot,
    width: u32,
    max_dim: u32,
}

impl LargeImageApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        // Just past the limit on whatever GPU this is → always ≥ 2 tiles wide.
        let max_dim = render_state.device.limits().max_texture_dimension_2d;
        let width = max_dim + 64;

        let image = build_image(width);
        set_image(render_state, &image);

        let mut plot = Plot::new(0);
        plot.limits = (0.0, image.width as f64, 0.0, image.height as f64);
        plot.colormap = image.colormap().cloned();

        Self {
            plot,
            width,
            max_dim,
        }
    }
}

impl eframe::App for LargeImageApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.label(format!(
                "image width {} px > max_texture_dimension_2d {} → tiled; gradient must stay continuous",
                self.width, self.max_dim
            ));
            PlotWidget::new().show(ui, &mut self.plot);
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "egui-silx · large_image",
        options,
        Box::new(|cc| Ok(Box::new(LargeImageApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
