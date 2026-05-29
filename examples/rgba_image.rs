//! RGBA image: display a per-pixel RGBA array directly, with no colormap (silx
//! `addImage` with an RGBA array).
//!
//! The image encodes red across x, green across y, a blue checkerboard, and a
//! radial alpha falloff (transparent corners), so the direct color path and the
//! per-pixel alpha blend are both visually checkable. No colorbar is shown,
//! since an RGBA image has no colormap.
//!
//! Run with: `cargo run --example rgba_image`

use eframe::egui;
use egui_silx::{ImageData, Plot, PlotWidget, install, set_image};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 192;

fn build_image() -> ImageData {
    let mut pixels = vec![[0u8; 4]; (WIDTH * HEIGHT) as usize];
    let cx = (WIDTH - 1) as f32 * 0.5;
    let cy = (HEIGHT - 1) as f32 * 0.5;
    let max_r = (cx * cx + cy * cy).sqrt();
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let r = (255 * col / (WIDTH - 1)) as u8; // red across x
            let g = (255 * row / (HEIGHT - 1)) as u8; // green across y
            let checker = ((col / 16) + (row / 16)) % 2 == 0;
            let b = if checker { 200 } else { 40 }; // blue checkerboard
            // Radial alpha: opaque at the center, transparent at the corners.
            let (dx, dy) = (col as f32 - cx, row as f32 - cy);
            let dist = (dx * dx + dy * dy).sqrt() / max_r; // 0..1
            let a = (255.0 * (1.0 - dist)).clamp(0.0, 255.0) as u8;
            pixels[(row * WIDTH + col) as usize] = [r, g, b, a];
        }
    }
    ImageData::rgba(WIDTH, HEIGHT, pixels)
}

struct RgbaApp {
    plot: Plot,
}

impl RgbaApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        let image = build_image();
        set_image(render_state, &image);

        let mut plot = Plot::new(0);
        plot.limits = (0.0, image.width as f64, 0.0, image.height as f64);
        // No colormap: an RGBA image carries its own colors (image.colormap() is None).
        plot.title = Some("rgba image (direct, per-pixel alpha)".to_owned());

        Self { plot }
    }
}

impl eframe::App for RgbaApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
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
        "egui-silx · rgba_image",
        options,
        Box::new(|cc| Ok(Box::new(RgbaApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
