//! Colormap normalization: the same wide-dynamic-range field rendered under
//! linear / log10 / sqrt / gamma / arcsinh normalization (silx
//! `Colormap.normalization`).
//!
//! Press 1·2·3·4·5 to switch normalization; the image and its colorbar update
//! together (the colorbar ticks move to where the image colors those values).
//! The field spans ~[1, 1000], so log/sqrt pull the low end up and gamma
//! darkens the midtones, all visibly different from linear.
//!
//! Run with: `cargo run --example colormap_norm`

use eframe::egui;
use egui::Key;
use rsplot::{Colormap, ImageData, Normalization, Plot, PlotView, install, set_image};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 192;
const VMIN: f64 = 1.0;
const VMAX: f64 = 1000.0;

/// A diagonal field rising exponentially from ~1 to ~1000, plus a bump.
fn build_field() -> Vec<f32> {
    let mut data = vec![0.0f32; (WIDTH * HEIGHT) as usize];
    for row in 0..HEIGHT {
        for col in 0..WIDTH {
            let u = col as f32 / (WIDTH - 1) as f32;
            let v = row as f32 / (HEIGHT - 1) as f32;
            let t = 0.5 * (u + v); // 0..1 diagonal
            let ramp = 10.0f32.powf(3.0 * t); // 1..1000
            let (du, dv) = (u - 0.3, v - 0.7);
            let bump = 1.0 + 400.0 * (-(du * du + dv * dv) / 0.01).exp();
            data[(row * WIDTH + col) as usize] = ramp.max(bump);
        }
    }
    data
}

fn colormap_for(norm: Normalization) -> Colormap {
    Colormap::viridis(VMIN, VMAX).with_normalization(norm)
}

fn norm_label(norm: Normalization) -> &'static str {
    match norm {
        Normalization::Linear => "linear",
        Normalization::Log => "log10",
        Normalization::Sqrt => "sqrt",
        Normalization::Gamma => "gamma 2.0",
        Normalization::Arcsinh => "arcsinh",
    }
}

struct NormApp {
    plot: Plot,
    data: Vec<f32>,
    norm: Normalization,
}

impl NormApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .expect("eframe must use the wgpu renderer (NativeOptions.renderer = Wgpu)");
        install(render_state);

        let data = build_field();
        let norm = Normalization::Linear;
        let image = ImageData::new(WIDTH, HEIGHT, data.clone(), colormap_for(norm));
        set_image(render_state, 0, &image);

        let mut plot = Plot::new(0);
        plot.limits = (0.0, WIDTH as f64, 0.0, HEIGHT as f64);
        plot.colormap = image.colormap().cloned();
        plot.title = Some(format!(
            "colormap normalization · {} (1·2·3·4·5)",
            norm_label(norm)
        ));

        Self { plot, data, norm }
    }

    /// Rebuild the image and colorbar under a new normalization.
    fn set_norm(&mut self, render_state: &eframe::egui_wgpu::RenderState, norm: Normalization) {
        if norm == self.norm {
            return;
        }
        self.norm = norm;
        let image = ImageData::new(WIDTH, HEIGHT, self.data.clone(), colormap_for(norm));
        set_image(render_state, 0, &image);
        self.plot.colormap = image.colormap().cloned();
        self.plot.title = Some(format!(
            "colormap normalization · {} (1·2·3·4·5)",
            norm_label(norm)
        ));
    }
}

impl eframe::App for NormApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let render_state = frame
            .wgpu_render_state()
            .expect("wgpu render state must be present");
        let next = ui.input(|i| {
            if i.key_pressed(Key::Num1) {
                Some(Normalization::Linear)
            } else if i.key_pressed(Key::Num2) {
                Some(Normalization::Log)
            } else if i.key_pressed(Key::Num3) {
                Some(Normalization::Sqrt)
            } else if i.key_pressed(Key::Num4) {
                Some(Normalization::Gamma)
            } else if i.key_pressed(Key::Num5) {
                Some(Normalization::Arcsinh)
            } else {
                None
            }
        });
        if let Some(norm) = next {
            self.set_norm(render_state, norm);
        }

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
        "rsplot · colormap_norm",
        options,
        Box::new(|cc| Ok(Box::new(NormApp::new(cc)) as Box<dyn eframe::App>)),
    )
}
